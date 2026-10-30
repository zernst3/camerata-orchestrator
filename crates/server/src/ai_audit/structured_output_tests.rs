//! Retry fixes F3, item 1: every JSON stage sends a response schema (`output_config.format`) on
//! the API backend, so a reply's shape and field types cannot be wrong, and keeps its tolerant
//! parser for the backends and models that never see the schema.
//!
//! No network: every model is a recorder that returns a canned reply.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::reply_schemas::check::{limit_violations, violation};
use super::reply_schemas::{
    alternatives_reply_schema, calibration_reply_schema, detail_reply_schema, fix_reply_schema,
    scan_reply_schema,
};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::run_store::journal::{CallFile, JournalLlm};
use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};

pub(super) fn response(text: &str, backend: &str, model: &str) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: model.to_string(),
        backend: backend.to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// Records every request and answers each stage with an empty (valid) reply.
pub(super) struct Recorder {
    log: Mutex<Vec<LlmRequest>>,
    backend: &'static str,
    model: &'static str,
    reply: Option<String>,
    calls: AtomicUsize,
}

impl Recorder {
    pub(super) fn new() -> Self {
        Self::on("api", "claude-sonnet-5-5")
    }

    fn on(backend: &'static str, model: &'static str) -> Self {
        Self {
            log: Mutex::new(Vec::new()),
            backend,
            model,
            reply: None,
            calls: AtomicUsize::new(0),
        }
    }

    pub(super) fn replying(mut self, text: &str) -> Self {
        self.reply = Some(text.to_string());
        self
    }

    pub(super) fn requests(&self) -> Vec<LlmRequest> {
        self.log.lock().unwrap().clone()
    }
}

/// Which stage a request belongs to, by its system prompt.
pub(super) fn stage_of(req: &LlmRequest) -> &'static str {
    let system = req.system.as_deref().unwrap_or("");
    if system.starts_with("You write the full explanation") {
        "detail"
    } else if system.starts_with("You write ONE concrete") {
        "fix"
    } else if system.starts_with("You are calibrating") {
        "calibration"
    } else if system.starts_with("You are a senior software architect helping") {
        "alternatives"
    } else if system.starts_with("You are a senior software architect") {
        "scan"
    } else {
        "other"
    }
}

#[async_trait::async_trait]
impl LlmPort for Recorder {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = self.reply.clone().unwrap_or_else(|| {
            match stage_of(&req) {
                "detail" => r#"{"details":[]}"#,
                "fix" => r#"{"fixes":[]}"#,
                "calibration" => r#"{"verdicts":[]}"#,
                "alternatives" => r#"{"recommendations":[]}"#,
                _ => r#"{"findings":[],"proposed_rules":[],"needs_files":[]}"#,
            }
            .to_string()
        });
        self.log.lock().unwrap().push(req);
        Ok(response(&text, self.backend, self.model))
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

pub(super) fn finding(path: &str, line: usize, detail: &str, snippet: &str) -> Finding {
    Finding {
        repo: "o/r".to_string(),
        path: path.to_string(),
        line,
        rule_id: "ARCH-SCHEMA-1".to_string(),
        severity: "medium".to_string(),
        snippet: snippet.to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

pub(super) fn files() -> Vec<(String, String)> {
    vec![(
        "src/work.ts".to_string(),
        "export function runWork(raw: string) {\n  return doWork(raw);\n}\n".to_string(),
    )]
}

fn nonce() -> crate::prompt_frame::FrameNonce {
    crate::prompt_frame::FrameNonce::generate_avoiding(&[])
}

fn alternatives(rule: &str) -> RuleAlternatives {
    RuleAlternatives {
        rule_id: rule.to_string(),
        options: vec![
            RuleOptionView {
                id: "opt-a".to_string(),
                label: "Option A".to_string(),
                directive: "Do it the A way.".to_string(),
                why: "A is simple.".to_string(),
                violates: false,
            },
            RuleOptionView {
                id: "opt-b".to_string(),
                label: "Option B".to_string(),
                directive: "Do it the B way.".to_string(),
                why: "B is scalable.".to_string(),
                violates: false,
            },
        ],
        selected_option_id: Some("opt-a".to_string()),
        kind: Some(camerata_rules::RuleKind::Decision),
    }
}

/// The ids a detail prompt asks about: the `[id]` that opens each finding's block.
fn prompt_ids(prompt: &str) -> Vec<String> {
    prompt
        .lines()
        .filter(|l| l.starts_with('[') && l.contains("] rule "))
        .filter_map(|l| l.find(']').map(|end| l[1..end].to_string()))
        .collect()
}

fn enum_of(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .expect("an enum")
        .iter()
        .map(|v| v.as_str().expect("a string id").to_string())
        .collect()
}

// ── the schemas ──────────────────────────────────────────────────────────────────────────────

/// Every schema is one the API accepts: closed objects, no length or range constraints, at most
/// 24 optional properties.
#[test]
fn every_reply_schema_respects_the_apis_limits() {
    let ids: Vec<String> = (0..10).map(|i| format!("{i:06x}")).collect();
    for (name, schema) in [
        ("scan", scan_reply_schema()),
        ("alternatives", alternatives_reply_schema()),
        ("calibration", calibration_reply_schema(40)),
        ("detail", detail_reply_schema(&ids)),
        ("fix", fix_reply_schema(&ids)),
    ] {
        let broken = limit_violations(&schema);
        assert!(broken.is_empty(), "{name}: {broken:?}");
    }
}

/// A reply in each stage's documented shape satisfies that stage's schema, and the stage's own
/// reader gets from it what it did from the same reply before: the schema path and the tolerant
/// path read one shape.
#[test]
fn replies_in_each_stages_shape_satisfy_its_schema_and_parse() {
    // Scan.
    let scan = serde_json::json!({
        "findings": [{
            "path": "src/a.ts", "line": 12, "severity": "high", "rule": "ARCH-SCHEMA-1",
            "title": "Handler skips the check", "code": "doThing(x)",
            "detail": "No guard before the call.", "captures": {"table": "invoices"}
        }],
        "proposed_rules": [{
            "name": "guard-writes", "title": "Guard every write", "rationale": "seen twice",
            "severity": "medium", "enforcement": "review"
        }],
        "needs_files": ["src/b.ts"]
    });
    assert_eq!(violation(&scan_reply_schema(), &scan), None);
    let adopted: std::collections::HashSet<String> =
        ["ARCH-SCHEMA-1".to_string()].into_iter().collect();
    let (f, p) = parse_ai_findings("o/r", &scan.to_string(), &adopted);
    assert_eq!((f.len(), p.len()), (1, 1));
    assert_eq!(
        f[0].captures.get("table").map(String::as_str),
        Some("invoices")
    );
    assert_eq!(parse_needs_files(&scan.to_string()), vec!["src/b.ts"]);
    // A clean scan, the minimal reply.
    let clean = serde_json::json!({"findings": [], "needs_files": []});
    assert_eq!(violation(&scan_reply_schema(), &clean), None);

    // Alternatives.
    let alt = serde_json::json!({"recommendations": [{
        "rule_id": "MULTI-1", "applicable": true, "recommended_option_id": "opt-b",
        "evidence": "src/a.ts:3 - uses B", "recommendation_reasoning": "B is used.",
        "sites_total": 4, "currently_followed": true,
        "observed_options": [{"option_id": "opt-b", "sites": 4}]
    }]});
    assert_eq!(violation(&alternatives_reply_schema(), &alt), None);
    let recs = parse_alternative_recommendations(&alt.to_string(), &[alternatives("MULTI-1")]);
    assert_eq!(recs[0].recommended_option_id, "opt-b");
    assert!(!recs[0].hallucinated);

    // Calibration.
    let cal = serde_json::json!({"verdicts": [{
        "index": 0, "severity": "low", "confidence": "high", "effort": "low",
        "category": "input-validation", "reason": "contained"
    }]});
    assert_eq!(violation(&calibration_reply_schema(3), &cal), None);
    let out = apply_verdicts(&cal.to_string(), vec![finding("a.ts", 1, "d", "s")]);
    assert_eq!(out[0].effort.as_deref(), Some("low"));
    // An index outside the batch is not a reply the schema allows.
    let outside = serde_json::json!({"verdicts": [{
        "index": 3, "severity": "low", "confidence": "high", "effort": "low", "reason": "x"
    }]});
    assert!(violation(&calibration_reply_schema(3), &outside).is_some());

    // Detail.
    let ids = vec!["a1b2c3".to_string(), "d4e5f6".to_string()];
    let detail = serde_json::json!({"details": [{"id": "a1b2c3", "detail": "Because."}]});
    assert_eq!(violation(&detail_reply_schema(&ids), &detail), None);
    let parsed = detail_regen::parse_detail_reply(&detail.to_string(), false);
    assert_eq!(parsed.entries.len(), 1);
    let foreign = serde_json::json!({"details": [{"id": "ffffff", "detail": "Because."}]});
    // The finding's location is not part of the reply; an echo is not a reply the schema allows.
    let echoing = serde_json::json!({"details": [{
        "id": "a1b2c3", "path": "src/a.ts", "line": 12, "detail": "Because."
    }]});
    assert!(violation(&detail_reply_schema(&ids), &echoing).is_some());
    assert!(violation(&detail_reply_schema(&ids), &foreign).is_some());

    // Fix.
    let fix = serde_json::json!({"fixes": [{"id": "d4e5f6", "fix": "Validate `raw`."}]});
    assert_eq!(violation(&fix_reply_schema(&ids), &fix), None);
    assert_eq!(parse_fix_specifics(&fix.to_string()).len(), 1);
    let foreign = serde_json::json!({"fixes": [{"id": "ffffff", "fix": "x"}]});
    assert!(violation(&fix_reply_schema(&ids), &foreign).is_some());
    // The fix stage may leave a finding out.
    assert_eq!(
        violation(&fix_reply_schema(&ids), &serde_json::json!({"fixes": []})),
        None
    );
}

// ── the requests ─────────────────────────────────────────────────────────────────────────────

/// The scan pass sends the scan schema; the same schema on every pass (a cached prefix is per
/// schema).
#[tokio::test]
async fn the_scan_request_carries_the_scan_schema() {
    let llm = Recorder::new();
    let adopted = std::collections::HashSet::new();
    for session in ["s-0", "s-1"] {
        let _ = audit_pass(
            &llm,
            None,
            "Repository: o/r\n\nthe digest".to_string(),
            Vec::new(),
            &nonce(),
            "o/r",
            &adopted,
            None,
            session,
            None,
            3,
        )
        .await
        .unwrap();
    }
    let reqs = llm.requests();
    assert_eq!(reqs.len(), 2);
    for r in &reqs {
        assert_eq!(stage_of(r), "scan");
        assert_eq!(r.response_schema, Some(scan_reply_schema()));
    }
}

/// The alternatives stage sends ONE schema for every rule batch (a different schema per batch
/// would invalidate the digest's cached prefix).
#[tokio::test]
async fn the_alternatives_requests_carry_one_schema_for_every_batch() {
    let llm = Recorder::new();
    let alts: Vec<RuleAlternatives> = (0..ALTERNATIVES_BATCH_SIZE + 3)
        .map(|i| alternatives(&format!("MULTI-{i}")))
        .collect();
    let _ = recommend_alternatives(
        &llm,
        "o/r",
        &files(),
        &files(),
        &nonce(),
        &alts,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let reqs: Vec<LlmRequest> = llm
        .requests()
        .into_iter()
        .filter(|r| stage_of(r) == "alternatives")
        .collect();
    assert!(reqs.len() >= 2, "two rule batches: {}", reqs.len());
    for r in &reqs {
        assert_eq!(r.response_schema, Some(alternatives_reply_schema()));
    }
}

/// Calibration sends a schema whose `index` enum is exactly the batch's positions.
#[tokio::test]
async fn the_calibration_request_carries_the_batch_size_in_its_schema() {
    let llm = Recorder::new();
    let findings: Vec<Finding> = (1..=3)
        .map(|i| finding("src/work.ts", i, "d", "doWork(raw)"))
        .collect();
    let _ = verify_findings(&llm, "o/r", findings, None, None, None, false, "").await;
    let reqs = llm.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(stage_of(&reqs[0]), "calibration");
    assert_eq!(reqs[0].response_schema, Some(calibration_reply_schema(3)));
    let indices = &reqs[0].response_schema.as_ref().unwrap()["properties"]["verdicts"]["items"]
        ["properties"]["index"]["enum"];
    assert_eq!(indices, &serde_json::json!([0, 1, 2]));
}

/// The detail request's `id` enum lists exactly the ids of the batch's findings, in the prompt.
#[tokio::test]
async fn the_detail_schemas_id_enum_is_exactly_the_batch_ids() {
    let llm = Recorder::new();
    let findings: Vec<Finding> = (1..=3)
        .map(|i| {
            finding(
                "src/work.ts",
                i,
                "Passes raw to doWork — Short statement.",
                "doWork(raw)",
            )
        })
        .collect();
    let (_out, _failed) = detail_regen::regenerate_full_details(
        &llm,
        "o/r",
        findings,
        &files(),
        &[],
        &nonce(),
        None,
        None,
    )
    .await;
    let reqs: Vec<LlmRequest> = llm
        .requests()
        .into_iter()
        .filter(|r| stage_of(r) == "detail")
        .collect();
    assert!(!reqs.is_empty());
    for r in &reqs {
        let schema = r.response_schema.as_ref().expect("a detail schema");
        let in_schema =
            enum_of(&schema["properties"]["details"]["items"]["properties"]["id"]["enum"]);
        assert_eq!(in_schema, prompt_ids(&r.prompt), "{}", r.prompt);
        assert!(!in_schema.is_empty());
    }
    // The first request is the full batch of three.
    assert_eq!(prompt_ids(&reqs[0].prompt).len(), 3);
}

/// The fix request's `id` enum is the batch's ids, the ones in its prompt.
#[tokio::test]
async fn the_fix_request_carries_the_batch_ids_in_its_schema() {
    let llm = Recorder::new();
    let findings = vec![
        finding(
            "src/work.ts",
            2,
            "`runWork` passes `raw` to `doWork`",
            "doWork(raw)",
        ),
        finding("src/work.ts", 1, "`runWork` takes `raw`", "runWork"),
    ];
    let _ = generate_fix_specifics(&llm, "o/r", findings, &files(), None, None, None).await;
    let first = llm
        .requests()
        .into_iter()
        .find(|r| stage_of(r) == "fix")
        .expect("a fix request");
    let ids = super::reply_schemas::check::prompt_ids(&first.prompt);
    assert_eq!(ids.len(), 2);
    assert_eq!(first.response_schema, Some(fix_reply_schema(&ids)));
}

// ── the wire ─────────────────────────────────────────────────────────────────────────────────

/// A request's schema is part of its identity: the same request with another schema, or with
/// none, is another key, and one without a schema keeps the key it always had.
#[test]
fn the_schema_is_part_of_the_request_key() {
    let base = LlmRequest::new("p").with_system("s");
    let plain = crate::run_store::journal::request_key(&base);
    let with_scan = crate::run_store::journal::request_key(
        &base.clone().with_response_schema(scan_reply_schema()),
    );
    let with_fix = crate::run_store::journal::request_key(
        &base
            .clone()
            .with_response_schema(fix_reply_schema(&["a1b2c3".to_string()])),
    );
    assert_ne!(plain, with_scan);
    assert_ne!(with_scan, with_fix);
    assert_eq!(
        with_scan,
        crate::run_store::journal::request_key(&base.with_response_schema(scan_reply_schema()))
    );
}

async fn seeded<F: std::future::Future>(fut: F) -> F::Output {
    crate::prompt_frame::with_seed(Some([7u8; crate::prompt_frame::NONCE_BYTES]), fut).await
}

fn new_run(repo_dir: &std::path::Path) -> Arc<RunStore> {
    RunStore::create(NewRun {
        project_key: "proj-1".to_string(),
        repos: vec![("o/r".to_string(), repo_dir.to_path_buf())],
        models: ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: AUDIT_PROMPT_SCHEMA_VERSION,
            directives_fingerprint: "fp".to_string(),
            terse_output: false,
            applies_to_routing: false,
        },
        args: serde_json::json!({}),
        estimate_usd: None,
        include_alternatives: false,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

fn call_files(store: &RunStore) -> Vec<CallFile> {
    std::fs::read_dir(store.dir().join("calls"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).ok())
        .collect()
}

/// The run store records which path produced a reply: `schema` when the API took the schema,
/// `tolerant` when the request carried one but the backend (the CLI) or the model could not,
/// nothing for a request with no schema.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_call_entry_records_which_path_produced_the_reply() {
    seeded(async {
        let state = tempfile::tempdir().unwrap();
        let _guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let repo = tempfile::tempdir().unwrap();
        for (backend, model, schema, expect) in [
            ("api", "claude-sonnet-5-5", true, Some("schema")),
            ("cli", "claude-sonnet-5-5", true, Some("tolerant")),
            ("api", "some-future-model", true, Some("tolerant")),
            ("api", "claude-sonnet-5-5", false, None),
        ] {
            let store = new_run(repo.path());
            let stub = Recorder::on(backend, model).replying(r#"{"findings":[]}"#);
            let journal = JournalLlm::new(&stub, store.clone());
            let mut req = LlmRequest::new("Repository: o/r\n\nx").with_system("sys");
            if schema {
                req = req.with_response_schema(scan_reply_schema());
            }
            let mut on_delta = |_: &str| {};
            journal
                .complete_streaming(req, &mut on_delta)
                .await
                .unwrap();
            let files = call_files(&store);
            assert_eq!(files.len(), 1);
            assert_eq!(
                files[0].entries[&0].output_path.as_deref(),
                expect,
                "{backend} {model} schema={schema}"
            );
        }
    })
    .await;
}

/// The tolerant path still reads today's replies: JSON inside a markdown fence with prose around
/// it, an alternative the schema would not allow (an unknown severity word), a verdict with no
/// effort. A model or backend that never sees the schema answers like this.
#[test]
fn the_tolerant_parsers_still_read_replies_the_schema_would_not_allow() {
    let fenced = "Here you go:\n```json\n{\"findings\":[{\"path\":\"a.ts\",\"line\":\"7\",\"severity\":\"Severe\",\"rule\":\"x\",\"title\":\"t\",\"code\":\"c\",\"detail\":\"d\"}]}\n```\nDone.";
    let (f, _) = parse_ai_findings("o/r", fenced, &std::collections::HashSet::new());
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].line, 7);
    assert_eq!(f[0].severity, "high");
    let verdicts =
        "```json\n{\"verdicts\":[{\"index\":0,\"severity\":\"low\",\"confidence\":\"low\"}]}\n```";
    let out = apply_verdicts(verdicts, vec![finding("a.ts", 1, "d", "s")]);
    assert_eq!(out[0].confidence.as_deref(), Some("needs-review"));
    let details = "{\"details\":[{\"id\":\"a1b2c3\",\"path\":\"p\",\"line\":1,\"detail\":\"Because.\"}]} trailing";
    assert_eq!(
        detail_regen::parse_detail_reply(details, false)
            .entries
            .len(),
        1
    );
    let fixes = "Sure.\n{\"fixes\":[{\"id\":\"a1b2c3\",\"fix\":\"Do it.\"}]}";
    assert_eq!(parse_fix_specifics(fixes).len(), 1);
}
