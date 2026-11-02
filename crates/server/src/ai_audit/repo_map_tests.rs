//! M1 item 1: the repo map keeps the complete file tree on every call and scopes only the
//! symbols.

use std::sync::Mutex;

use super::repo_map::{estimate_tokens, REPO_TREE_TOKEN_BUDGET, WHOLE_MAP_TOKEN_BUDGET};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

fn nonce() -> crate::prompt_frame::FrameNonce {
    crate::prompt_frame::FrameNonce::fixed("0123456789abcdef0123456789abcdef")
}

/// `n` generated files spread over directories, each defining a few public symbols.
fn synthetic_files(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| {
            let dir = format!("src/area{}/module{}", i % 25, (i / 25) % 40);
            (
                format!("{dir}/component_{i}.ts"),
                format!(
                    "export class Component{i} {{}}\nexport function helper{i}() {{}}\n\
                     export const value{i} = 1;\n"
                ),
            )
        })
        .collect()
}

fn small_repo() -> Vec<(String, String)> {
    vec![
        (
            "src/lib.rs".to_string(),
            "pub struct Engine;\npub fn run() {}\n".to_string(),
        ),
        (
            "src/util.rs".to_string(),
            "pub fn helper() {}\n".to_string(),
        ),
        ("README.md".to_string(), "# readme\n".to_string()),
    ]
}

#[test]
fn a_small_repo_map_is_byte_identical_to_the_old_map() {
    let files = small_repo();
    let map = RepoMap::new(&files, &nonce(), REPO_TREE_TOKEN_BUDGET);
    assert_eq!(map.prefix(), build_repo_map(&files, &nonce()));
    assert!(map.tree_fallback().is_none());
    // The whole map already carries every symbol: no symbols section.
    let section = map.symbols_for(&files[..1], &nonce());
    assert_eq!(section.text, "");
    assert_eq!(section.files_with_symbols, 3);
    assert_eq!(section.outlined_files, 0);
}

#[test]
fn the_old_map_is_pinned_for_a_known_repo() {
    // The exact bytes of the pre-M1 map, so the extraction into repo_map.rs cannot drift.
    let files = small_repo();
    let expected = format!(
        "REPO MAP — every file in the repo and the public symbols it defines. Only SOME file \
         bodies appear in THIS pass; use this map for cross-file architectural context (which \
         directories are repositories vs services vs controllers, where a named type lives):\n\
         <<<UNTRUSTED {n} \"repo map\">>>\n  src/lib.rs  [Engine, run]\n  src/util.rs  [helper]\n  \
         README.md\n<<<END UNTRUSTED {n}>>>\n\n",
        n = nonce()
    );
    assert_eq!(build_repo_map(&files, &nonce()), expected);
}

#[test]
fn a_big_repo_gets_the_complete_path_only_tree() {
    let files = synthetic_files(3000);
    assert!(
        estimate_tokens(build_repo_map(&files, &nonce()).len()) > WHOLE_MAP_TOKEN_BUDGET,
        "the fixture must be too big for the whole map"
    );
    let map = RepoMap::new(&files, &nonce(), REPO_TREE_TOKEN_BUDGET);
    assert!(map.tree_fallback().is_none());
    let prefix = map.prefix();
    // Every file appears: its directory line, then its name under it.
    for (path, _) in &files {
        let (dir, name) = path.rsplit_once('/').unwrap();
        let block_start = prefix
            .find(&format!("\n{dir}/\n"))
            .unwrap_or_else(|| panic!("directory {dir} missing"));
        let block = &prefix[block_start + 1..];
        let block_end = block[1..].find("\n\n").map_or(block.len(), |e| e + 1);
        let block = &block[..block_end];
        let block = block.split("\n\n").next().unwrap();
        assert!(
            block.contains(&format!("\n  {name}\n")) || block.ends_with(&format!("\n  {name}")),
            "{path} missing under its directory"
        );
    }
    // Paths only: no symbols in the tree.
    assert!(!prefix.contains("Component1"));
    assert!(!prefix.contains('['));
}

/// A repo big enough for the tree, with the controller / command layout of a typical app.
fn controller_repo() -> Vec<(String, String)> {
    let mut files = synthetic_files(2500);
    files.push((
        "src/controllers/OrderController.ts".to_string(),
        "import { Order } from '../models/Order';\nexport class OrderController {}\n".to_string(),
    ));
    files.push((
        "src/models/Order.ts".to_string(),
        "export class Order {}\n".to_string(),
    ));
    files.push((
        "commands/CreateOrderCommand.ts".to_string(),
        "export class CreateOrderCommand {}\n".to_string(),
    ));
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

struct Capture {
    prompts: Mutex<Vec<(String, Vec<usize>)>>,
}

#[async_trait::async_trait]
impl LlmPort for Capture {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.prompts
            .lock()
            .unwrap()
            .push((req.prompt.clone(), req.cache_breakpoints.clone()));
        Ok(LlmResponse {
            text: "{\"findings\":[]}".to_string(),
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn scan_chunks(map: &RepoMap, chunks: &[&[(String, String)]]) -> Vec<(String, Vec<usize>)> {
    let llm = Capture {
        prompts: Mutex::new(Vec::new()),
    };
    let rules = vec![("RULE-A".to_string(), "do a".to_string())];
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let (_f, _p, _r, ok, err) = run_passes(
        &llm,
        "acme/widgets",
        map,
        &nonce(),
        &std::collections::HashSet::new(),
        None,
        None,
        None,
        chunks,
        None,
        &batches,
        1,
        "pass[all]",
        "m1",
        None,
        false,
    )
    .await;
    assert!(err.is_empty(), "{err:?}");
    assert_eq!(ok, chunks.len());
    llm.prompts.into_inner().unwrap()
}

#[tokio::test]
async fn a_controller_chunk_still_sees_the_command_file_by_its_exact_path() {
    let files = controller_repo();
    let map = RepoMap::new(&files, &nonce(), REPO_TREE_TOKEN_BUDGET);
    assert!(map.tree_fallback().is_none());
    let chunk: Vec<(String, String)> = files
        .iter()
        .filter(|(p, _)| p == "src/controllers/OrderController.ts")
        .cloned()
        .collect();
    // The command file is in no neighbour relation to the controller (other directory, no import).
    let section = map.symbols_for(&chunk, &nonce());
    assert!(
        !section.text.contains("CreateOrderCommand"),
        "the command file must not be a neighbour in this test"
    );
    let prompts = scan_chunks(&map, &[chunk.as_slice()]).await;
    let (prompt, _) = &prompts[0];
    // Its directory line, then its name under it: the exact path, and the model may ask for it.
    assert!(
        prompt.contains("\ncommands/\n  CreateOrderCommand.ts\n"),
        "the tree must name commands/CreateOrderCommand.ts"
    );
    // The controller's import is a neighbour, so its symbols are there.
    assert!(prompt.contains("src/models/Order.ts  [Order]"));
}

#[tokio::test]
async fn the_tree_is_identical_on_every_call_and_before_the_cache_breakpoint() {
    let files = controller_repo();
    let map = RepoMap::new(&files, &nonce(), REPO_TREE_TOKEN_BUDGET);
    let chunk_a: Vec<(String, String)> = files[..40].to_vec();
    let chunk_b: Vec<(String, String)> = files[2000..2040].to_vec();
    let prompts = scan_chunks(&map, &[chunk_a.as_slice(), chunk_b.as_slice()]).await;
    assert_eq!(prompts.len(), 2);
    let lead = format!("Repository: acme/widgets\n\n{}", map.prefix());
    for (prompt, bps) in &prompts {
        assert!(prompt.starts_with(&lead), "the tree leads the prompt");
        assert!(
            bps.first().is_some_and(|bp| *bp == lead.len()),
            "the first cache breakpoint closes the tree: {bps:?}"
        );
        // The symbols section comes after the breakpoint.
        let symbols_at = prompt.find("SYMBOLS — ").expect("symbols section");
        assert!(symbols_at >= lead.len());
    }
    // Byte-identical tree on both calls.
    assert_eq!(
        &prompts[0].0[..lead.len()],
        &prompts[1].0[..lead.len()],
        "the tree bytes differ between calls"
    );
    // And the symbols differ per chunk (they are not in the cached part).
    assert_ne!(
        prompts[0].0[lead.len()..],
        prompts[1].0[lead.len()..],
        "chunk-specific content follows the breakpoint"
    );
}

#[test]
fn symbols_cover_the_chunk_its_directory_and_its_imports_only() {
    let files = controller_repo();
    let map = RepoMap::new(&files, &nonce(), REPO_TREE_TOKEN_BUDGET);
    let chunk: Vec<(String, String)> = files
        .iter()
        .filter(|(p, _)| p == "src/controllers/OrderController.ts")
        .cloned()
        .collect();
    let s = map.symbols_for(&chunk, &nonce());
    assert!(s.text.contains("OrderController"));
    assert!(s.text.contains("src/models/Order.ts"), "an import");
    assert!(!s.text.contains("Component7"), "an unrelated file");
    assert_eq!(s.files_with_symbols, 2);
    assert_eq!(s.files_in_map, files.len());

    // A file that imports the chunk's file is a neighbour too.
    let chunk: Vec<(String, String)> = files
        .iter()
        .filter(|(p, _)| p == "src/models/Order.ts")
        .cloned()
        .collect();
    let s = map.symbols_for(&chunk, &nonce());
    assert!(s.text.contains("src/controllers/OrderController.ts"));
}

#[test]
fn the_tree_of_5000_files_stays_within_its_budget() {
    let files = synthetic_files(5000);
    let map = RepoMap::new(&files, &nonce(), REPO_TREE_TOKEN_BUDGET);
    assert!(map.tree_fallback().is_none());
    let tokens = estimate_tokens(map.prefix().len());
    eprintln!(
        "tree of 5000 files: {} bytes, {tokens} estimated tokens",
        map.prefix().len()
    );
    assert!(tokens < REPO_TREE_TOKEN_BUDGET / 2);
}

#[test]
fn a_tree_over_the_budget_falls_back_to_an_outline_and_says_so() {
    let files = synthetic_files(3000);
    // A budget the path-only tree cannot meet.
    let map = RepoMap::new(&files, &nonce(), 2_000);
    let fallback = map.tree_fallback().expect("the fallback is recorded");
    assert_eq!(fallback.files, 3000);
    assert!(fallback.tree_tokens > fallback.budget);
    assert!(estimate_tokens(map.prefix().len()) <= 2_000 + 200);
    assert!(map.prefix().contains("outline"));
    assert!(map.prefix().contains("file"));
    let disclosure = fallback.disclosure();
    assert!(disclosure.contains("3000 files"), "{disclosure}");
    assert!(!disclosure.contains('—'), "no em dashes in report text");
    // A chunk's call still names its own files and neighbours by exact path.
    let chunk = files[..3].to_vec();
    let s = map.symbols_for(&chunk, &nonce());
    assert!(s.text.contains(&files[0].0));
    assert_eq!(s.outlined_files, 3000);
}

#[test]
fn the_fallback_is_recorded_in_the_telemetry_and_a_call_records_its_map() {
    use crate::stage_telemetry::{RepoMapCall, RepoMapTelemetry, RepoTreeFallback, StageMeter};
    let meter = StageMeter::default();
    meter.record_repo_map(RepoMapCall {
        repo: "r".into(),
        group: "pass[all]".into(),
        chunk: 1,
        files_in_map: 10,
        files_with_symbols: 4,
        outlined_files: 0,
    });
    meter.record_tree_fallback(RepoTreeFallback {
        repo: "r".into(),
        files: 10,
        tree_tokens: 300,
        budget: 100,
    });
    let snap = meter.snapshot(0);
    let maps: &RepoMapTelemetry = snap.repo_maps.as_ref().expect("recorded");
    assert_eq!(maps.calls.len(), 1);
    assert_eq!(maps.calls[0].files_with_symbols, 4);
    assert_eq!(maps.fallbacks.len(), 1);
    let text = snap.render_table();
    assert!(text.contains("repo map FALLBACK"), "{text}");
}
