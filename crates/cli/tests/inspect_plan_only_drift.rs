#![allow(clippy::unwrap_used)]
//! The plan-only drift test, part two (#358): `camerata inspect --plan-only` and a real run
//! (`run_inspect_pipeline_with`) over the same two repositories agree on what is read, what is not
//! read and why, and what each piece holds, piece by piece in the real run's own order.
//!
//! No model is called: the stages the fixture reaches run on a scripted model, and the agent
//! finder's agent is a recorder that notes what each piece was given and then fails the piece, so
//! the real run's own failure records name every piece by its place ("part 2 of 3").
//!
//! The first part of the drift test (the pre-flight text, the piece count and the pause under a
//! small limit) is in `inspect_plan_only.rs`.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::inspect_cmd::finder::FinderArgs;
use camerata::inspect_cmd::plan_only::run_plan_only_with;
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, InspectError, Seams};
use camerata_server::ai_audit::agent_finder::run::with_agent_override;
use camerata_server::ai_audit::agent_finder::{
    AgentRun, AgentStop, FinderChoice, FollowUp, PieceAgent, PieceLimits, Room,
};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

/// The exclusion pattern the fixture's `seeds` folder is excluded by.
const EXCLUDE: &str = "*/seeds/**";

fn put(root: &Path, rel: &str, content: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn git(dest: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dest)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Four large files of different lengths in one folder and a small shared one, so the agent
/// finder cuts the repository into two pieces or more and no two pieces have the same figures (a
/// swap of two pieces shows).
fn stage_big_repo(dest: &Path) {
    put(dest, "package.json", br#"{"name":"svc","version":"1.0.0"}"#);
    for f in 0..4 {
        let mut body = String::from("import { shared } from './shared';\n");
        for i in 0..(2_300 + 150 * f) {
            body.push_str(&format!(
                "export const value_{f}_{i} = (x: number) => shared(x) + {f} * {i};\n"
            ));
        }
        put(dest, &format!("src/part{f}.ts"), body.as_bytes());
    }
    put(
        dest,
        "src/shared.ts",
        b"export const shared = (x: number) => x;\n",
    );
}

/// A small git repository with three files the run reads and one file for each reason a file is
/// named as not read that a fixture can make: over the size limit, a line too long for the room,
/// not UTF-8 text, declares itself generated, a type the inspection does not read yet, a type
/// opened for declared versions only, excluded by agreement, a symbolic link, and a tracked name
/// that is not UTF-8 text.
fn stage_mixed_repo(dest: &Path) {
    put(
        dest,
        "app/main.ts",
        b"import { helper } from './helper';\nexport const run = () => helper();\n",
    );
    put(dest, "app/helper.ts", b"export const helper = () => 1;\n");
    put(dest, "app/routes.ts", b"export const routes = ['/'];\n");
    put(dest, "app/big.ts", &vec![b'x'; 1_100_000]);
    put(
        dest,
        "app/wide.ts",
        format!("export const wide = '{}';\n", "w".repeat(120_000)).as_bytes(),
    );
    put(dest, "app/latin.ts", b"export const caf\xe9 = 'caf\xe9';\n");
    put(
        dest,
        "app/schema.ts",
        b"// @generated\nexport const s = 1;\n",
    );
    put(dest, "views/Index.cshtml", b"<h1>@Model.Title</h1>\n");
    put(
        dest,
        "App.csproj",
        b"<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n",
    );
    put(dest, "seeds/data.ts", b"export const seed = 1;\n");
    std::os::unix::fs::symlink("main.ts", dest.join("app/link.ts")).unwrap();
    {
        use std::os::unix::ffi::OsStrExt as _;
        let name = std::ffi::OsStr::from_bytes(b"app/odd\xe9.ts");
        std::fs::write(dest.join(name), b"export const odd = 1;\n").unwrap();
    }
    git(dest, &["init", "-q", "-b", "main"]);
    git(dest, &["config", "user.email", "drift@example.test"]);
    git(dest, &["config", "user.name", "Plan Drift"]);
    git(dest, &["config", "core.quotepath", "false"]);
    git(dest, &["add", "-A"]);
    git(dest, &["commit", "-q", "-m", "fixture"]);
}

fn args(repos: &[(&Path, &str)], export: &Path) -> InspectArgs {
    InspectArgs {
        repo: repos[0].0.to_path_buf(),
        also_repos: repos[1..].iter().map(|(p, _)| p.to_path_buf()).collect(),
        repo_names: repos.iter().map(|(_, n)| (*n).to_string()).collect(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        // No baseline is read, so a run saved by an earlier call cannot change what this one does.
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        // An internal run writes its export although a piece failed, so its Coverage can be read.
        client_engagement: false,
        run_client_tooling: false,
        consensus: false,
        finder: FinderArgs {
            finder: FinderChoice::Agent,
            ..FinderArgs::default()
        },
        exclude: vec![EXCLUDE.to_string()],
    }
}

/// A model for the stages the fixture reaches; the finder's agent is the recorder below.
struct Quiet;

#[async_trait]
impl LlmPort for Quiet {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.01),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// What one piece holds, in the figures a plan line prints.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PieceSeen {
    files: usize,
    lines: usize,
    bytes: usize,
    context_files: usize,
    context_bytes: usize,
    checklist_bytes: usize,
    turn_limit: usize,
}

/// An agent that records what each piece is given under the number of its call, then fails the
/// piece with that number in the reason, so the run's failure record for "part i of n" says which
/// call it was.
#[derive(Default)]
struct Recorder {
    seen: Mutex<BTreeMap<usize, (Vec<String>, PieceSeen)>>,
    calls: AtomicUsize,
}

#[async_trait]
impl PieceAgent for Recorder {
    async fn run(
        &self,
        room: &Room,
        _brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let held = room.file_pairs();
        let of = |paths: &[String], f: &dyn Fn(&str) -> usize| -> usize {
            held.iter()
                .filter(|(p, _)| paths.contains(p))
                .map(|(_, t)| f(t))
                .sum()
        };
        let checklist = room
            .read_plain("_camerata/checklist.md", 1, usize::MAX)
            .map(|r| r.text.len())
            .unwrap_or(0);
        let seen = PieceSeen {
            files: room.piece_files().len(),
            lines: of(room.piece_files(), &|t| t.lines().count()),
            bytes: of(room.piece_files(), &str::len),
            context_files: room.context_files().len(),
            context_bytes: of(room.context_files(), &str::len),
            checklist_bytes: checklist,
            turn_limit: limits.max_turns,
        };
        self.seen
            .lock()
            .unwrap()
            .insert(call, (room.piece_files().to_vec(), seen));
        Ok(AgentRun::empty(AgentStop::Failed(format!(
            "recorded call {call}"
        ))))
    }
    async fn follow_up(
        &self,
        _room: &Room,
        _prior: &AgentRun,
        _turn: &FollowUp,
        _limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        anyhow::bail!("not used")
    }
}

/// What a real run did: per repository, the pieces in the run's own order (from its failure
/// records), the files of every piece, and the export's Coverage rows (repo, reason, path).
struct RealRun {
    pieces: BTreeMap<String, Vec<(Vec<String>, PieceSeen)>>,
    not_read: Vec<(String, String, String)>,
}

async fn real_run(repos: &[(&Path, &str)]) -> RealRun {
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let recorder = Arc::new(Recorder::default());
    let result = Box::pin(with_agent_override(
        recorder.clone(),
        run_inspect_pipeline_with(
            args(repos, &export),
            true,
            &camerata_server::credentials::MemoryCredentialStore::new(),
            Seams {
                resume: None,
                llm: Some(&Quiet),
                preflight_sink: &|_| {},
            },
        ),
    ))
    .await;
    let outcome = match result {
        Ok(o) => o,
        Err(e) => panic!("an internal run exports although a piece failed: {e:?}"),
    };
    let seen = recorder.seen.lock().unwrap().clone();

    // `camerata failure: class=.. repo=<repo> part="agent finder, part <i> of <n>" .. message="the
    // review failed: recorded call <k>"`: the run's own place for each piece.
    let mut placed: BTreeMap<String, BTreeMap<usize, usize>> = BTreeMap::new();
    for line in &outcome.failure_lines {
        let field = |key: &str| -> Option<&str> {
            let start = line.find(key)? + key.len();
            let rest = &line[start..];
            Some(&rest[..rest.find([' ', '"']).unwrap_or(rest.len())])
        };
        let Some(part) = line
            .split("part=\"agent finder, part ")
            .nth(1)
            .and_then(|r| r.split(' ').next())
        else {
            continue;
        };
        let repo = field("repo=").unwrap().to_string();
        let call: usize = line
            .split("recorded call ")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .unwrap_or_else(|| panic!("no call number in the failure record: {line}"))
            .parse()
            .unwrap();
        placed
            .entry(repo)
            .or_default()
            .insert(part.parse().unwrap(), call);
    }
    let pieces: BTreeMap<String, Vec<(Vec<String>, PieceSeen)>> = placed
        .into_iter()
        .map(|(repo, by_place)| {
            let ordered = by_place.values().map(|call| seen[call].clone()).collect();
            (repo, ordered)
        })
        .collect();
    assert_eq!(
        pieces.values().map(Vec::len).sum::<usize>(),
        seen.len(),
        "every piece the agent saw has a failure record with its place:\n{:#?}",
        outcome.failure_lines
    );

    let bytes = std::fs::read(&export).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut raw = String::new();
    archive
        .by_name("json/inspection.json")
        .unwrap()
        .read_to_string(&mut raw)
        .unwrap();
    let inspection: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let not_read = inspection["coverage"]["skipped_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["repo"].as_str().unwrap().to_string(),
                row["reason"].as_str().unwrap().to_string(),
                row["path"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    RealRun { pieces, not_read }
}

/// What the plan printed for one repository.
#[derive(Debug, Default)]
struct PlannedRepo {
    admitted: usize,
    left_out: BTreeMap<String, usize>,
    pieces: Vec<PieceSeen>,
}

/// `1,234` back to 1234.
fn num(s: &str) -> usize {
    s.trim().replace(',', "").parse().unwrap()
}

/// The text between `before` and `after` in `line`.
fn between<'a>(line: &'a str, before: &str, after: &str) -> &'a str {
    let start = line
        .find(before)
        .unwrap_or_else(|| panic!("{before:?} in {line:?}"))
        + before.len();
    let rest = &line[start..];
    &rest[..rest
        .find(after)
        .unwrap_or_else(|| panic!("{after:?} after {before:?} in {line:?}"))]
}

/// Read the admission and piece sections of a plan back into figures, repository by repository.
fn read_plan(text: &str) -> BTreeMap<String, PlannedRepo> {
    let mut out: BTreeMap<String, PlannedRepo> = BTreeMap::new();
    let mut section = "";
    let mut repo = String::new();
    for line in text.lines() {
        if line.starts_with("== ") {
            section = line;
            continue;
        }
        if section.contains("ADMISSION") {
            if let Some(rest) = line.strip_prefix("    left out: ") {
                if rest != "none" {
                    let (count, reason) = rest.split_once(' ').unwrap();
                    out.get_mut(&repo)
                        .unwrap()
                        .left_out
                        .insert(reason.to_string(), num(count));
                }
            } else if let Some((name, rest)) = line
                .strip_prefix("  ")
                .and_then(|l| l.split_once(": "))
                .filter(|(name, _)| *name != "total")
            {
                repo = name.to_string();
                out.entry(repo.clone()).or_default().admitted =
                    num(between(rest, "", " files admitted"));
            }
        } else if section.contains("PIECES") {
            if let Some(name) = line.strip_prefix(' ').and_then(|l| l.strip_suffix(':')) {
                repo = name.to_string();
            } else if line.starts_with("  piece ") {
                out.get_mut(&repo).unwrap().pieces.push(PieceSeen {
                    files: num(between(line, "): ", " files,")),
                    lines: num(between(line, " files, ", " lines,")),
                    bytes: num(between(line, " lines, ", " bytes;")),
                    context_files: num(between(line, "; context ", " files,")),
                    context_bytes: num(between(
                        &line[line.find("; context ").unwrap()..],
                        " files, ",
                        " bytes;",
                    )),
                    checklist_bytes: num(between(line, "checklist ", " bytes;")),
                    turn_limit: num(&line[line.find("turn limit ").unwrap() + 11..]),
                });
            }
        }
    }
    out
}

fn on_a_big_stack<F: std::future::Future<Output = ()> + Send + 'static>(fut: F) {
    std::thread::Builder::new()
        .stack_size(128 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(fut);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Over two repositories, one cut into several pieces and one with a file for every reason a file
/// is not read: the plan admits as many files as the real run's pieces hold, names as many files
/// not read under each reason as the real run's Coverage, and prints each repository's pieces in
/// the real run's order with the same files, lines, bytes, context, checklist and turn limit.
#[test]
#[serial_test::serial(process_env)]
fn the_plan_matches_a_real_run_on_admission_several_repos_and_piece_order() {
    on_a_big_stack(async {
        let state = tempfile::tempdir().unwrap();
        let _state =
            camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path().to_str().unwrap());
        let _no_keychain = camerata_test_env::EnvGuard::set("CAMERATA_NO_KEYCHAIN", "1");
        let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        let root = tempfile::tempdir().unwrap();
        let api = root.path().join("api");
        let web = root.path().join("web");
        stage_big_repo(&api);
        stage_mixed_repo(&web);
        let repos = [(api.as_path(), "acme/api"), (web.as_path(), "acme/web")];
        let out = tempfile::tempdir().unwrap();

        let printed = Mutex::new(String::new());
        run_plan_only_with(args(&repos, &out.path().join("inspect.zip")), true, &|t| {
            printed.lock().unwrap().push_str(t);
        })
        .await
        .expect("nothing blocks");
        let text = printed.into_inner().unwrap();
        let planned = read_plan(&text);
        let real = real_run(&repos).await;

        // Both repositories, in both, and the fixture is what it says it is.
        assert_eq!(
            planned.keys().collect::<Vec<_>>(),
            ["acme/api", "acme/web"],
            "{text}"
        );
        assert_eq!(
            real.pieces.keys().collect::<Vec<_>>(),
            ["acme/api", "acme/web"],
            "the real run reviewed both repositories"
        );
        assert!(
            real.pieces["acme/api"].len() >= 2,
            "the big repository should be cut in two or more"
        );
        let api_pieces = &planned["acme/api"].pieces;
        for (i, a) in api_pieces.iter().enumerate() {
            assert!(
                !api_pieces[i + 1..].contains(a),
                "no two pieces of the big repository may have the same figures, or a swap would \
                 not show:\n{text}"
            );
        }

        for (repo, plan) in &planned {
            let pieces = &real.pieces[repo];

            // Admission: what the plan says is read is what the real run's pieces hold, file by
            // file once each.
            let mut held: Vec<&String> = pieces.iter().flat_map(|(files, _)| files).collect();
            held.sort();
            let before = held.len();
            held.dedup();
            assert_eq!(before, held.len(), "{repo}: a file sits in two pieces");
            assert_eq!(
                plan.admitted,
                held.len(),
                "{repo}: the plan admits a different number of files than the real run reads \
                 ({held:?}):\n{text}"
            );

            // Not read: the same count under every reason Coverage gives.
            let mut real_left_out: BTreeMap<String, usize> = BTreeMap::new();
            for (r, reason, _) in &real.not_read {
                if r == repo {
                    *real_left_out.entry(reason.clone()).or_default() += 1;
                }
            }
            assert_eq!(
                plan.left_out, real_left_out,
                "{repo}: the files named as not read differ by reason (plan, then real run; \
                 the real rows are {:#?}):\n{text}",
                real.not_read
            );

            // Pieces: the same figures in the same order.
            let real_figures: Vec<&PieceSeen> = pieces.iter().map(|(_, s)| s).collect();
            assert_eq!(
                plan.pieces.iter().collect::<Vec<_>>(),
                real_figures,
                "{repo}: the pieces differ, or are in a different order (plan, then real \
                 run):\n{text}"
            );
        }
        // The mixed repository names a file under each reason the fixture plants.
        assert!(
            planned["acme/web"].left_out.len() >= 8,
            "the mixed repository should name files under at least eight reasons:\n{text}"
        );
    });
}
