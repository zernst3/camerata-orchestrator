#![allow(clippy::unwrap_used)]
//! A zero-token rehearsal of a whole scan: 13 neutral repositories of mixed languages, the real
//! run through `run_inspect_pipeline_with`, and every model call answered by a script.
//!
//! * the finder is the REAL API transport (`ApiPieceAgent`) over a scripted wire, built on the
//!   run's own shared meter through the `test-support` factory seam;
//! * the stages after it (fix, group review, duplicate check) run on a scripted model and a fake
//!   Message Batches API ([`BatchSeam`]);
//! * nothing touches a network or a keychain: the proxy variables point at a closed port, so a
//!   real HTTP client that was built for a provider address by mistake would fail at once.
//!
//! What it plays, in order, on one run saved to disk:
//!
//! 1. a 429 burst on a batch submit (the stage waits the provider's `retry-after`);
//! 2. a batch that stays "in progress" for many polls;
//! 3. a credit-error row in a batch (the run stops and the row is not sent again);
//! 4. a spend-limit stop, then a resume to completion on batches;
//! 5. the export and the release gate.
//!
//! It asserts that every admitted file reaches a piece, that Coverage names every file left out
//! with its reason, that no problem is lost across the stop and the resume, that the run's spend
//! is exactly the sum of what the scripts reported, and that the export opens and agrees with the
//! run. Peak resident memory and wall time are printed, never asserted.
//!
//! Two sizes share one body: a small one in the normal suite, and the full size (about 200K
//! lines) behind `--ignored` because it takes minutes.
//!
//! The fixture is built in a temp folder (the test's `TMPDIR`). One name here cannot be built on
//! this machine's file system: a tracked file whose name is not UTF-8 (APFS refuses to create
//! one). It is put into the index only, in its own test at the bottom.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use camerata::inspect_cmd::finder::FinderArgs;
use camerata::inspect_cmd::{
    resume_inspect, run_inspect_pipeline_with, InspectArgs, InspectError, InspectOutcome, Seams,
};
use camerata_server::ai_audit::agent_finder::api_agent::scripted::{ScriptedFailure, ScriptedWire};
use camerata_server::ai_audit::agent_finder::api_agent::ApiPieceAgent;
use camerata_server::ai_audit::agent_finder::brief_text::JOINTS_BRIEF;
use camerata_server::ai_audit::agent_finder::run::{
    with_agent_factory_override, AgentFactoryOverride,
};
use camerata_server::ai_audit::agent_finder::{
    AgentRun, FinderChoice, FollowUp, PieceAgent, PieceLimits, Room, DEFAULT_FINDER_MODEL,
};
use camerata_server::ai_audit::{BatchApi, BatchSeam, UsageMeter};
use camerata_server::failure::{AttemptRecord, FailureClass, LlmCallFailure};
use camerata_server::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse, ProjectBackend,
};
use camerata_server::run_store::journal::classify;
use camerata_server::run_store::PartKind;
use camerata_server::scan_ledger::{ScanLedger, SkipReason};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------------------------
// What the scripts charge. Every scripted reply carries its own dollar figure, so the run's
// total spend is a sum that can be computed here, never estimated.
// ---------------------------------------------------------------------------------------------

const FINDER_TURN_MICRO: u64 = 20_000;
const REALTIME_CALL_MICRO: u64 = 10_000;
const BATCH_ROW_MICRO: u64 = 5_000;

const CREDIT_ERROR: &str =
    "Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing.";

/// Text that exists only outside every repository. A tracked symbolic link points at it. If it
/// ever appears in a request the finder sends, a link was read through.
const OUTSIDE_SENTINEL: &str = "OUTSIDE-SENTINEL-6d1f0c2e";

/// The money the scripts reported, in micro-dollars.
#[derive(Default)]
struct Spend(AtomicU64, [AtomicU64; 3]);

impl Spend {
    fn add(&self, micro: u64) {
        self.0.fetch_add(micro, Ordering::SeqCst);
        let slot = match micro {
            FINDER_TURN_MICRO => 0,
            REALTIME_CALL_MICRO => 1,
            _ => 2,
        };
        self.1[slot].fetch_add(micro, Ordering::SeqCst);
    }
    /// What the finder, the real-time calls and the batch rows were charged, in dollars.
    fn by_kind(&self) -> [f64; 3] {
        [0, 1, 2].map(|i| self.1[i].load(Ordering::SeqCst) as f64 / 1_000_000.0)
    }
    fn micro(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn usd(&self) -> f64 {
        self.micro() as f64 / 1_000_000.0
    }
}

// ---------------------------------------------------------------------------------------------
// The scale
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Scale {
    name: &'static str,
    /// Divides every repository's line count.
    divisor: usize,
    /// Lines in one source file.
    file_lines: usize,
    force_pieces: Option<usize>,
    /// Polls a slow batch answers "in progress" before it ends.
    slow_polls: usize,
    /// Submits answered with a 429 before one is accepted.
    burst: usize,
    /// Finder sends answered with a 429 or a 529 before the first good reply.
    finder_hiccups: usize,
    /// The fewest pieces the large repository must split into.
    core_min_pieces: usize,
}

const SMALL: Scale = Scale {
    name: "small",
    divisor: 40,
    file_lines: 50,
    force_pieces: Some(3),
    slow_polls: 3,
    burst: 3,
    finder_hiccups: 4,
    core_min_pieces: 3,
};

const SMALL_FAULTS: Faults = Faults {
    finder_credit_after: Some(40),
    batch_credit_row: false,
    limit_usd: Some(0.6),
};

const FULL_FAULTS: Faults = Faults {
    finder_credit_after: Some(150),
    batch_credit_row: false,
    limit_usd: Some(4.0),
};

const FULL: Scale = Scale {
    name: "full",
    divisor: 1,
    file_lines: 200,
    force_pieces: None,
    slow_polls: 8,
    burst: 3,
    finder_hiccups: 6,
    core_min_pieces: 8,
};

// ---------------------------------------------------------------------------------------------
// The fixture: 13 repositories, about 200K lines at full size
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    Js,
    CSharp,
    Java,
    Go,
    Ruby,
    Php,
}

impl Lang {
    fn ext(self) -> &'static str {
        match self {
            Lang::Rust => "rs",
            Lang::Python => "py",
            Lang::Js => "js",
            Lang::CSharp => "cs",
            Lang::Java => "java",
            Lang::Go => "go",
            Lang::Ruby => "rb",
            Lang::Php => "php",
        }
    }

    fn comment(self) -> &'static str {
        match self {
            Lang::Python | Lang::Ruby => "#",
            _ => "//",
        }
    }

    /// Statement templates. `{v}` and `{i}` are names, `{a}` and `{b}` numbers, `{n}` a unique
    /// number. A `\n` inside a template is a line break at the same indent plus its own spaces.
    fn statements(self) -> [&'static str; 8] {
        match self {
            Lang::Rust => [
                "let mut {v} = {a} + {b};",
                "if {v} > {a} {\n    {v} -= {b};\n}",
                "for {i} in 0..{a} {\n    {v} += {i} * {b};\n}",
                "{v} = helper_{n}({v}, {b});",
                "println!(\"step {n} {{}}\", {v});",
                "let {i} = {v}.min({a}).max({b});",
                "match {v} % {b} {\n    0 => {v} += 1,\n    _ => {v} -= 1,\n}",
                "assert!({v} >= 0, \"negative at {n}\");",
            ],
            Lang::Python => [
                "{v} = {a} + {b}",
                "if {v} > {a}:\n    {v} -= {b}",
                "for {i} in range({a}):\n    {v} += {i} * {b}",
                "{v} = helper_{n}({v}, {b})",
                "print('step {n}', {v})",
                "{i} = min(max({v}, {b}), {a})",
                "while {v} > {a} * {b}:\n    {v} //= 2",
                "assert {v} >= 0, 'negative at {n}'",
            ],
            Lang::Js => [
                "let {v} = {a} + {b};",
                "if ({v} > {a}) {\n  {v} -= {b};\n}",
                "for (let {i} = 0; {i} < {a}; {i}++) {\n  {v} += {i} * {b};\n}",
                "{v} = helper{n}({v}, {b});",
                "console.log('step {n}', {v});",
                "const {i} = Math.min(Math.max({v}, {b}), {a});",
                "switch ({v} % {b}) {\n  case 0: {v} += 1; break;\n  default: {v} -= 1;\n}",
                "if ({v} < 0) throw new Error('negative at {n}');",
            ],
            Lang::CSharp => [
                "var {v} = {a} + {b};",
                "if ({v} > {a})\n{\n    {v} -= {b};\n}",
                "for (var {i} = 0; {i} < {a}; {i}++)\n{\n    {v} += {i} * {b};\n}",
                "{v} = Helper{n}({v}, {b});",
                "Console.WriteLine(\"step {n} \" + {v});",
                "var {i} = Math.Min(Math.Max({v}, {b}), {a});",
                "switch ({v} % {b})\n{\n    case 0: {v} += 1; break;\n    default: {v} -= 1; break;\n}",
                "if ({v} < 0) throw new InvalidOperationException(\"negative at {n}\");",
            ],
            Lang::Java => [
                "int {v} = {a} + {b};",
                "if ({v} > {a}) {\n    {v} -= {b};\n}",
                "for (int {i} = 0; {i} < {a}; {i}++) {\n    {v} += {i} * {b};\n}",
                "{v} = helper{n}({v}, {b});",
                "System.out.println(\"step {n} \" + {v});",
                "int {i} = Math.min(Math.max({v}, {b}), {a});",
                "switch ({v} % {b}) {\n    case 0: {v} += 1; break;\n    default: {v} -= 1;\n}",
                "if ({v} < 0) throw new IllegalStateException(\"negative at {n}\");",
            ],
            Lang::Go => [
                "{v} := {a} + {b}",
                "if {v} > {a} {\n\t{v} -= {b}\n}",
                "for {i} := 0; {i} < {a}; {i}++ {\n\t{v} += {i} * {b}\n}",
                "{v} = helper{n}({v}, {b})",
                "fmt.Println(\"step {n}\", {v})",
                "{i} := min(max({v}, {b}), {a})",
                "switch {v} % {b} {\ncase 0:\n\t{v}++\ndefault:\n\t{v}--\n}",
                "if {v} < 0 {\n\tpanic(\"negative at {n}\")\n}",
            ],
            Lang::Ruby => [
                "{v} = {a} + {b}",
                "if {v} > {a}\n  {v} -= {b}\nend",
                "{a}.times do |{i}|\n  {v} += {i} * {b}\nend",
                "{v} = helper_{n}({v}, {b})",
                "puts \"step {n} #{{v}}\"",
                "{i} = [[{v}, {b}].max, {a}].min",
                "case {v} % {b}\nwhen 0 then {v} += 1\nelse {v} -= 1\nend",
                "raise 'negative at {n}' if {v} < 0",
            ],
            Lang::Php => [
                "${v} = {a} + {b};",
                "if (${v} > {a}) {\n    ${v} -= {b};\n}",
                "for (${i} = 0; ${i} < {a}; ${i}++) {\n    ${v} += ${i} * {b};\n}",
                "${v} = helper{n}(${v}, {b});",
                "echo 'step {n} ' . ${v};",
                "${i} = min(max(${v}, {b}), {a});",
                "switch (${v} % {b}) {\n    case 0: ${v} += 1; break;\n    default: ${v} -= 1;\n}",
                "if (${v} < 0) { throw new Exception('negative at {n}'); }",
            ],
        }
    }

    /// The first and last line of a function, and the indent of its body.
    fn function(self, name: &str) -> (String, Option<&'static str>, &'static str) {
        match self {
            Lang::Rust => (
                format!("pub fn {name}(input: i64) -> i64 {{"),
                Some("}"),
                "    ",
            ),
            Lang::Python => (format!("def {name}(input):"), None, "    "),
            Lang::Js => (format!("function {name}(input) {{"), Some("}"), "  "),
            Lang::CSharp => (
                format!("public static long {name}(long input) {{"),
                Some("}"),
                "    ",
            ),
            Lang::Java => (
                format!("static long {name}(long input) {{"),
                Some("}"),
                "    ",
            ),
            Lang::Go => (format!("func {name}(input int) int {{"), Some("}"), "\t"),
            Lang::Ruby => (format!("def {name}(input)"), Some("end"), "  "),
            Lang::Php => (format!("function {name}($input) {{"), Some("}"), "    "),
        }
    }
}

/// A small deterministic generator, so a fixture is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Exactly `n_lines` non-blank lines of plausible code in `lang`, seeded by `seed`.
fn render_file(lang: Lang, seed: u64, n_lines: usize) -> String {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let templates = lang.statements();
    let mut lines: Vec<String> = vec![format!("{} acme module {seed}", lang.comment())];
    if lang == Lang::Php {
        lines[0] = "<?php".to_string();
    }
    if lang == Lang::Go {
        lines.push("package acme".to_string());
    }
    let mut f = 0u64;
    while lines.len() < n_lines {
        f += 1;
        let (head, tail, indent) = lang.function(&format!("handle_{seed}_{f}"));
        lines.push(head);
        let statements = 3 + rng.below(6);
        for _ in 0..statements {
            let template = templates[rng.below(8) as usize];
            let text = template
                .replace("{v}", &format!("v{}", rng.below(9)))
                .replace("{i}", &format!("i{}", rng.below(4)))
                .replace("{a}", &(2 + rng.below(900)).to_string())
                .replace("{b}", &(1 + rng.below(97)).to_string())
                .replace("{n}", &rng.below(100_000).to_string());
            for line in text.split('\n') {
                lines.push(format!("{indent}{line}"));
            }
        }
        lines.push(format!("{indent}return input;"));
        if let Some(tail) = tail {
            lines.push(tail.to_string());
        }
    }
    lines.truncate(n_lines);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

struct RepoSpec {
    name: &'static str,
    lang: Lang,
    /// Lines at full size.
    lines: usize,
}

const REPOS: [RepoSpec; 13] = [
    RepoSpec {
        name: "core",
        lang: Lang::Rust,
        lines: 80_000,
    },
    RepoSpec {
        name: "api",
        lang: Lang::CSharp,
        lines: 12_000,
    },
    RepoSpec {
        name: "web",
        lang: Lang::Js,
        lines: 12_000,
    },
    RepoSpec {
        name: "ui",
        lang: Lang::Js,
        lines: 8_000,
    },
    RepoSpec {
        name: "billing",
        lang: Lang::Java,
        lines: 12_000,
    },
    RepoSpec {
        name: "ledger",
        lang: Lang::Go,
        lines: 10_000,
    },
    RepoSpec {
        name: "etl",
        lang: Lang::Python,
        lines: 12_000,
    },
    RepoSpec {
        name: "ml",
        lang: Lang::Python,
        lines: 8_000,
    },
    RepoSpec {
        name: "mail",
        lang: Lang::Ruby,
        lines: 8_000,
    },
    RepoSpec {
        name: "cms",
        lang: Lang::Php,
        lines: 10_000,
    },
    RepoSpec {
        name: "auth",
        lang: Lang::Go,
        lines: 8_000,
    },
    RepoSpec {
        name: "sdk",
        lang: Lang::Java,
        lines: 10_000,
    },
    RepoSpec {
        name: "tools",
        lang: Lang::Python,
        lines: 10_000,
    },
];

struct Fixture {
    /// (name as the run shows it, folder).
    repos: Vec<(String, PathBuf)>,
    /// Source files the run should admit, and their lines.
    admitted_files: usize,
    admitted_lines: usize,
    /// Files the run must name as left out: repo, path, reason.
    left_out: Vec<(String, String, SkipReason)>,
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn build_fixture(root: &Path, scale: &Scale) -> Fixture {
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let outside_file = outside.join("notes.txt");
    std::fs::write(
        &outside_file,
        format!("{OUTSIDE_SENTINEL}\nnot part of any repository\n"),
    )
    .unwrap();

    let mut fx = Fixture {
        repos: Vec::new(),
        admitted_files: 0,
        admitted_lines: 0,
        left_out: Vec::new(),
    };
    for (r, spec) in REPOS.iter().enumerate() {
        let dir = root.join(format!("acme-{}", spec.name));
        let display = format!("acme/{}", spec.name);
        let total = (spec.lines / scale.divisor).max(scale.file_lines * 6);
        let files = total.div_ceil(scale.file_lines);
        for i in 0..files {
            let lines = scale.file_lines.min(total - i * scale.file_lines);
            let at = dir.join(format!("src/mod_{}/file_{i}.{}", i % 8, spec.lang.ext()));
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(
                &at,
                render_file(spec.lang, (r as u64) * 100_000 + i as u64, lines),
            )
            .unwrap();
            fx.admitted_files += 1;
            fx.admitted_lines += lines;
        }
        match spec.name {
            // One file over the 1 MiB limit.
            "api" => {
                let mut big = String::new();
                let mut n = 0u64;
                while big.len() < 1_300_000 {
                    big.push_str(&format!("    public const int Entry{n} = {n};\n"));
                    n += 1;
                }
                std::fs::create_dir_all(dir.join("data")).unwrap();
                std::fs::write(dir.join("data/lookup_table.cs"), big).unwrap();
                fx.left_out.push((
                    display.clone(),
                    "data/lookup_table.cs".into(),
                    SkipReason::Oversize,
                ));
            }
            // Two tracked symbolic links: one out of the repository, one inside it.
            "web" => {
                std::os::unix::fs::symlink(&outside_file, dir.join("outside_link.js")).unwrap();
                std::os::unix::fs::symlink("src/mod_0/file_0.js", dir.join("latest.js")).unwrap();
                fx.left_out.push((
                    display.clone(),
                    "outside_link.js".into(),
                    SkipReason::Symlink,
                ));
                fx.left_out
                    .push((display.clone(), "latest.js".into(), SkipReason::Symlink));
            }
            // A file in a legacy code page: not UTF-8 text.
            "mail" => {
                std::fs::create_dir_all(dir.join("legacy")).unwrap();
                let mut bytes = b"# caf\xe9 notes in a legacy code page\n".to_vec();
                bytes.extend_from_slice(b"def old_handler(x)\n  x\nend\n");
                std::fs::write(dir.join("legacy/notes.rb"), bytes).unwrap();
                fx.left_out.push((
                    display.clone(),
                    "legacy/notes.rb".into(),
                    SkipReason::NotText,
                ));
            }
            _ => {}
        }
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "rehearsal@camerata.local"]);
        git(&dir, &["config", "user.name", "Rehearsal"]);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "fixture"]);
        fx.repos.push((display, dir));
    }
    fx
}

// ---------------------------------------------------------------------------------------------
// The scripted model for the stages under the finder, and the fake batch API behind it
// ---------------------------------------------------------------------------------------------

struct Model {
    spend: Arc<Spend>,
    calls: AtomicUsize,
}

impl Model {
    /// The model's answer to a request, as a pure function of the request.
    fn answer(&self, req: &LlmRequest, micro: u64) -> LlmResponse {
        let (kind, _repo, _files) = classify(req);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = match kind {
            PartKind::Calibration => json!({
                "severity": "medium", "real": "yes", "reason": "checked", "effort": "low",
            })
            .to_string(),
            PartKind::Fix => {
                // A fix is addressed by the opaque id its block opens with, which is also the
                // label of the block's frame: `<<<UNTRUSTED token "finding a1b2c3">>>`.
                let fixes: Vec<Value> = req
                    .prompt
                    .lines()
                    .filter(|l| l.starts_with("<<<UNTRUSTED "))
                    .filter_map(|l| l.split("\"finding ").nth(1))
                    .filter_map(|rest| rest.split('"').next())
                    .map(|id| json!({"id": id, "fix": "Check the input before it is used."}))
                    .collect();
                json!({ "fixes": fixes }).to_string()
            }
            PartKind::GroupReview => camerata_server::ai_audit::group_review::stand_in_reply(
                req.system.as_deref(),
                &req.prompt,
            )
            .unwrap_or_default(),
            _ => r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#.to_string(),
        };
        LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(micro as f64 / 1_000_000.0),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        }
    }
}

#[async_trait]
impl LlmPort for Model {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.spend.add(REALTIME_CALL_MICRO);
        Ok(self.answer(&req, REALTIME_CALL_MICRO))
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

fn request_of(item: &BatchItem) -> LlmRequest {
    let text = match item.params.messages[0].content.as_str() {
        Some(t) => t.to_string(),
        None => item.params.messages[0]
            .content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>(),
    };
    let mut req = LlmRequest::new(text);
    req.system.clone_from(&item.params.system);
    req
}

/// A provider's 429, as the engine's own transport reports it: typed, with the `retry-after`.
fn rate_limited(retry_after_secs: u64) -> anyhow::Error {
    let mut record = AttemptRecord::new("api", "batch");
    record.http_status = Some(429);
    record.error_type = Some("rate_limit_error".to_string());
    record.message = "This request would exceed your rate limit".to_string();
    record.retry_after_secs = Some(retry_after_secs);
    let mut failure = LlmCallFailure::new(
        FailureClass::RateLimited,
        "Anthropic API HTTP 429 Too Many Requests: rate_limit_error",
    );
    failure.attempts = vec![record];
    failure.retryable = true;
    failure.http = Some((429, "rate_limit_error".to_string()));
    anyhow::Error::new(failure)
}

/// The fake Message Batches API: it answers every item as the scripted model would, and plays
/// the faults of the rehearsal.
struct Batches {
    model: Model,
    spend: Arc<Spend>,
    items: Mutex<HashMap<String, Vec<BatchItem>>>,
    /// The kind and custom id of every item of every submit that was accepted, in order.
    accepted: Mutex<Vec<(PartKind, String)>>,
    submits_ok: AtomicUsize,
    // 1. the 429 burst
    burst_left: AtomicUsize,
    burst_played: AtomicUsize,
    // 2. the slow batch: the second accepted batch stays in progress for `slow_polls` polls
    slow_polls: usize,
    polls: Mutex<HashMap<String, usize>>,
    // 3. the credit row: the first item of the third batch that holds fix requests
    credit_armed: AtomicBool,
    fix_batches: AtomicUsize,
    credit_batch: Mutex<Option<String>>,
    credit_custom_id: Mutex<Option<String>>,
    // each (batch, item) is billed once, however often its rows are fetched
    billed: Mutex<HashSet<(String, String)>>,
}

impl Batches {
    fn new(spend: Arc<Spend>, scale: &Scale) -> Self {
        Self {
            model: Model {
                spend: spend.clone(),
                calls: AtomicUsize::new(0),
            },
            spend,
            items: Mutex::new(HashMap::new()),
            accepted: Mutex::new(Vec::new()),
            submits_ok: AtomicUsize::new(0),
            burst_left: AtomicUsize::new(scale.burst),
            burst_played: AtomicUsize::new(0),
            slow_polls: scale.slow_polls,
            polls: Mutex::new(HashMap::new()),
            credit_armed: AtomicBool::new(true),
            fix_batches: AtomicUsize::new(0),
            credit_batch: Mutex::new(None),
            credit_custom_id: Mutex::new(None),
            billed: Mutex::new(HashSet::new()),
        }
    }

    fn times_accepted(&self, custom_id: &str) -> usize {
        self.accepted
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, id)| id == custom_id)
            .count()
    }
}

#[async_trait]
impl BatchApi for Batches {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &self.model
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        Some(1_000)
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        // 1. A burst of 429s on the first submits: nothing is accepted, the stage waits.
        if self
            .burst_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            self.burst_played.fetch_add(1, Ordering::SeqCst);
            return Err(rate_limited(1));
        }
        let n = self.submits_ok.fetch_add(1, Ordering::SeqCst);
        let id = format!("msgbatch_rehearsal_{n}");
        let kinds: Vec<(PartKind, String)> = items
            .iter()
            .map(|i| (classify(&request_of(i)).0, i.custom_id.clone()))
            .collect();
        if kinds.iter().any(|(k, _)| *k == PartKind::Fix) {
            let ordinal = self.fix_batches.fetch_add(1, Ordering::SeqCst) + 1;
            if ordinal == 3 && self.credit_armed.load(Ordering::SeqCst) {
                *self.credit_batch.lock().unwrap() = Some(id.clone());
                *self.credit_custom_id.lock().unwrap() = kinds
                    .iter()
                    .find(|(k, _)| *k == PartKind::Fix)
                    .map(|(_, c)| c.clone());
            }
        }
        self.accepted.lock().unwrap().extend(kinds);
        self.items.lock().unwrap().insert(id.clone(), items);
        Ok(id)
    }
    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        // 2. The second batch stays in progress for many polls.
        let polled = {
            let mut polls = self.polls.lock().unwrap();
            let seen = polls.entry(batch_id.to_string()).or_insert(0);
            *seen += 1;
            *seen
        };
        let slow = batch_id.ends_with("_1") && polled <= self.slow_polls;
        Ok(BatchStatus {
            processing_status: if slow { "in_progress" } else { "ended" }.to_string(),
            request_counts: BatchRequestCounts {
                processing: u32::from(slow),
                ..BatchRequestCounts::default()
            },
        })
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        let items = self
            .items
            .lock()
            .unwrap()
            .get(batch_id)
            .cloned()
            .unwrap_or_default();
        let credit_target = {
            let batch = self.credit_batch.lock().unwrap();
            if batch.as_deref() == Some(batch_id) && self.credit_armed.load(Ordering::SeqCst) {
                self.credit_custom_id.lock().unwrap().clone()
            } else {
                None
            }
        };
        let mut rows = Vec::new();
        for item in &items {
            if credit_target.as_deref() == Some(item.custom_id.as_str()) {
                // 3. A credit error on one row. The credit comes back for the resume.
                self.credit_armed.store(false, Ordering::SeqCst);
                rows.push(BatchResultRow {
                    custom_id: item.custom_id.clone(),
                    response: None,
                    error: Some(CREDIT_ERROR.to_string()),
                    billed: None,
                });
                continue;
            }
            if self
                .billed
                .lock()
                .unwrap()
                .insert((batch_id.to_string(), item.custom_id.clone()))
            {
                self.spend.add(BATCH_ROW_MICRO);
            }
            let mut response = self.model.answer(&request_of(item), BATCH_ROW_MICRO);
            response.backend = "api/batch".to_string();
            rows.push(BatchResultRow {
                custom_id: item.custom_id.clone(),
                response: Some(response),
                error: None,
                billed: None,
            });
        }
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------------------------
// The real finder transport over a scripted wire
// ---------------------------------------------------------------------------------------------

/// What every scripted wire of the run shares.
struct Shared {
    spend: Arc<Spend>,
    /// Sends still to be answered with a 429 or a 529, one after another.
    hiccups_left: AtomicUsize,
    hiccups_played: AtomicUsize,
    /// Waits the transport asked for (all answered at once).
    pauses: AtomicUsize,
    /// Requests the wire answered well.
    turns: AtomicUsize,
    /// The largest request body seen, in bytes.
    largest_body: AtomicUsize,
    sentinel_seen: AtomicBool,
    /// The account runs out of credit after this many good replies (usize::MAX: never).
    credit_after: AtomicUsize,
    credit_errors: AtomicUsize,
    /// (path, quoted line) of every problem the script wrote. The first line of each fixture file
    /// holds its own seed, so this names one problem in one file of one repository.
    problems: Mutex<BTreeSet<(String, String)>>,
    /// Pieces the script ran, by the kind of call.
    piece_runs: AtomicUsize,
    joints_runs: AtomicUsize,
}

struct ScriptWire {
    /// One entry per model turn: the tool calls the model makes in it.
    turns: Vec<Vec<Value>>,
    shared: Arc<Shared>,
}

fn message_reply(content: Vec<Value>, stop_reason: &str, input: u64, output: u64) -> LlmResponse {
    let v = json!({
        "id": "msg_rehearsal", "type": "message", "role": "assistant",
        "model": DEFAULT_FINDER_MODEL, "content": content, "stop_reason": stop_reason,
        "usage": {
            "input_tokens": input, "output_tokens": output,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0
        }
    });
    LlmResponse {
        text: v.to_string(),
        model: DEFAULT_FINDER_MODEL.to_string(),
        backend: "anthropic/api/agentic".to_string(),
        cost_usd: Some(FINDER_TURN_MICRO as f64 / 1_000_000.0),
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: None,
    }
}

#[async_trait]
impl ScriptedWire for ScriptWire {
    async fn send(&self, body: &Value, _use_caching: bool) -> Result<LlmResponse, ScriptedFailure> {
        // A burst of 429s and 529s at the start: the transport waits and sends the same body.
        if self
            .shared
            .hiccups_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            let n = self.shared.hiccups_played.fetch_add(1, Ordering::SeqCst);
            let (status, kind) = if n.is_multiple_of(2) {
                (429, "rate_limit_error")
            } else {
                (529, "overloaded_error")
            };
            return Err(ScriptedFailure::Http {
                status,
                body: json!({"type": "error", "error": {"type": kind, "message": "slow down"}})
                    .to_string(),
                retry_after: n.is_multiple_of(2).then(|| Duration::from_secs(2)),
            });
        }
        // The credit runs out: the provider answers 400 with its own words, every time.
        if self.shared.turns.load(Ordering::SeqCst)
            >= self.shared.credit_after.load(Ordering::SeqCst)
        {
            self.shared.credit_errors.fetch_add(1, Ordering::SeqCst);
            return Err(ScriptedFailure::Http {
                status: 400,
                body: json!({"type": "error", "error": {
                    "type": "invalid_request_error",
                    "message": CREDIT_ERROR,
                }})
                .to_string(),
                retry_after: None,
            });
        }
        let text = body.to_string();
        self.shared
            .largest_body
            .fetch_max(text.len(), Ordering::SeqCst);
        if text.contains(OUTSIDE_SENTINEL) {
            self.shared.sentinel_seen.store(true, Ordering::SeqCst);
        }
        let assistant_turns = body["messages"]
            .as_array()
            .map_or(0, |m| m.iter().filter(|x| x["role"] == "assistant").count());
        self.shared.spend.add(FINDER_TURN_MICRO);
        self.shared.turns.fetch_add(1, Ordering::SeqCst);
        Ok(match self.turns.get(assistant_turns) {
            Some(calls) => {
                let blocks: Vec<Value> = calls
                    .iter()
                    .enumerate()
                    .map(|(k, c)| {
                        let mut call = c.clone();
                        call["id"] = json!(format!("toolu_{assistant_turns}_{k}"));
                        call
                    })
                    .collect();
                message_reply(blocks, "tool_use", 20_000, 800)
            }
            None => message_reply(
                vec![json!({"type": "text", "text": "Done."})],
                "end_turn",
                20_000,
                200,
            ),
        })
    }
}

fn tool(name: &str, input: Value) -> Value {
    json!({"type": "tool_use", "name": name, "input": input})
}

/// The script for one call: every Read it needs to cover its files, the problems, the checklist
/// answers, the card and the list of unread files. Computed from the room the run built.
fn script_for(room: &Room, joints: bool, shared: &Shared) -> Vec<Vec<Value>> {
    let write = |file: &str, text: String| {
        tool(
            "Write",
            json!({"file_path": format!("_camerata/out/{file}"), "content": text}),
        )
    };
    if joints {
        // The joints call: nothing across parts to report, and every lead (there are none)
        // answered.
        return vec![vec![
            write("problems-1.json", "[]".to_string()),
            write("leads-answers.json", json!({"answers": []}).to_string()),
        ]];
    }
    let counts = room.piece_line_counts();
    // Every read, paged as the room pages it, so each file is covered in full.
    let mut reads: Vec<Value> = Vec::new();
    for (path, lines) in &counts {
        let mut first = 1usize;
        while first <= *lines {
            let out = room.read(path, first, 2_000).unwrap();
            if out.lines_returned == 0 {
                break;
            }
            reads.push(tool(
                "Read",
                json!({"file_path": path, "offset": first, "limit": 2_000}),
            ));
            first += out.lines_returned;
        }
    }
    let mut turns: Vec<Vec<Value>> = reads.chunks(8).map(<[Value]>::to_vec).collect();

    // Two problems: at the first line of the first two files, quoting that line.
    let mut problems: Vec<Value> = Vec::new();
    for (n, (path, _)) in counts.iter().take(2).enumerate() {
        let page = room.read(path, 1, 50).unwrap().text;
        let Some((line, code)) = page.lines().find_map(|l| {
            let (number, text) = l.split_once('\t')?;
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            Some((number.trim().parse::<usize>().ok()?, text.to_string()))
        }) else {
            continue;
        };
        let title = format!("Input is not checked in {path}");
        shared
            .problems
            .lock()
            .unwrap()
            .insert((path.clone(), code.clone()));
        problems.push(json!({
            "n": n + 1, "path": path, "line": line, "end_line": line, "severity": "high",
            "title": title, "code": code,
            "what": "The function trusts the value its caller passes in.",
            "why": "A caller can pass a value the function does not expect.",
            "business": "The product's data can quietly go wrong.",
            "fix": "Check the value before it is used.", "sure": "yes", "effort": "low",
            "label": "SEC-NO-RAW-SQL-CONCAT-1"
        }));
    }
    turns.push(vec![write(
        "problems-1.json",
        Value::Array(problems).to_string(),
    )]);

    // The checklist: opened after the problems, page by page (it is plain text, 2,000 lines to a
    // page), then every rule answered clean at a line read.
    let mut checklist = String::new();
    let mut checklist_reads: Vec<Value> = Vec::new();
    let mut page_first = 1usize;
    loop {
        let page = room
            .read_plain("_camerata/checklist.md", page_first, 2_000)
            .unwrap();
        if page.lines_returned == 0 {
            break;
        }
        checklist.push_str(&page.text);
        checklist_reads.push(tool(
            "Read",
            json!({"file_path": "_camerata/checklist.md", "offset": page_first, "limit": 2_000}),
        ));
        page_first += page.lines_returned;
        if page_first > page.total_lines {
            break;
        }
    }
    let first = counts.first().map(|(p, _)| p.clone()).unwrap_or_default();
    let answers: Vec<Value> = checklist
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .filter_map(|l| l.split_once(": ").map(|(id, _)| id.to_string()))
        .map(|id| json!({"rule": id, "answer": "clean", "places": [{"path": first, "line": 1}]}))
        .collect();
    turns.extend(checklist_reads.chunks(8).map(<[Value]>::to_vec));
    turns.push(vec![
        write(
            "checklist.json",
            json!({"citations": [], "answers": answers}).to_string(),
        ),
        write("card.md", "It trusts its callers.".to_string()),
        write("unread.json", "[]".to_string()),
    ]);
    turns
}

/// The agent the run is given for each repository: the REAL API transport, a new one for each
/// call (each over its own script), all on the run's shared meter.
type PauseFn = Arc<dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

struct Rehearsal {
    meter: Arc<UsageMeter>,
    shared: Arc<Shared>,
    kept: Mutex<HashMap<String, Arc<ApiPieceAgent>>>,
}

impl Rehearsal {
    fn agent_over(&self, turns: Vec<Vec<Value>>) -> Arc<ApiPieceAgent> {
        let wire = Arc::new(ScriptWire {
            turns,
            shared: self.shared.clone(),
        });
        let shared = self.shared.clone();
        // Every wait is answered at once and counted.
        let pause: PauseFn = Arc::new(move |_| {
            shared.pauses.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        });
        Arc::new(
            ApiPieceAgent::new(DEFAULT_FINDER_MODEL, "sk-ant-rehearsal-not-a-real-key")
                .with_meter(self.meter.clone())
                .with_scripted_wire(wire)
                .with_pause(pause),
        )
    }
}

#[async_trait]
impl PieceAgent for Rehearsal {
    async fn run(
        &self,
        room: &Room,
        brief: &str,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        let joints = brief.starts_with(JOINTS_BRIEF);
        if joints {
            self.shared.joints_runs.fetch_add(1, Ordering::SeqCst);
        } else {
            self.shared.piece_runs.fetch_add(1, Ordering::SeqCst);
        }
        let agent = self.agent_over(script_for(room, joints, &self.shared));
        let run = agent.run(room, brief, limits).await?;
        self.kept
            .lock()
            .unwrap()
            .insert(run.conversation.as_str().to_string(), agent);
        Ok(run)
    }

    async fn follow_up(
        &self,
        room: &Room,
        prior: &AgentRun,
        turn: &FollowUp,
        limits: &PieceLimits,
    ) -> anyhow::Result<AgentRun> {
        let agent = self
            .kept
            .lock()
            .unwrap()
            .get(prior.conversation.as_str())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no conversation kept for this handle"))?;
        let run = agent.follow_up(room, prior, turn, limits).await?;
        self.kept
            .lock()
            .unwrap()
            .insert(run.conversation.as_str().to_string(), agent);
        Ok(run)
    }

    fn release(&self, conversation: &camerata_server::ai_audit::agent_finder::ConversationHandle) {
        if let Some(agent) = self.kept.lock().unwrap().remove(conversation.as_str()) {
            agent.release(conversation);
        }
    }

    fn meters_its_own_turns(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------------------------
// Peak memory, the run, and the checks
// ---------------------------------------------------------------------------------------------

/// Samples this process's resident memory with `ps` until stopped; the peak is in KiB.
struct MemorySampler {
    stop: Arc<AtomicBool>,
    peak_kib: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MemorySampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak_kib = Arc::new(AtomicU64::new(0));
        let (s, p) = (stop.clone(), peak_kib.clone());
        let handle = std::thread::spawn(move || {
            let pid = std::process::id().to_string();
            while !s.load(Ordering::SeqCst) {
                if let Ok(out) = std::process::Command::new("ps")
                    .args(["-o", "rss=", "-p", &pid])
                    .output()
                {
                    if let Ok(kib) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                        p.fetch_max(kib, Ordering::SeqCst);
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        Self {
            stop,
            peak_kib,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.peak_kib.load(Ordering::SeqCst)
    }
}

/// Run `fut` on a runtime with a large stack: the whole pipeline is one deep chain of futures.
fn on_a_big_stack<F: Future<Output = ()> + Send + 'static>(fut: F) {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
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

fn store() -> camerata_server::credentials::MemoryCredentialStore {
    camerata_server::credentials::MemoryCredentialStore::new()
}

fn inspect_args(fx: &Fixture, export: &Path, scale: &Scale) -> InspectArgs {
    InspectArgs {
        repo: fx.repos[0].1.clone(),
        also_repos: fx.repos[1..].iter().map(|(_, p)| p.clone()).collect(),
        repo_names: fx.repos.iter().map(|(n, _)| n.clone()).collect(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: true,
        realtime: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        // The maintainer's own run: an incomplete run still writes an internal export.
        client_engagement: false,
        run_client_tooling: false,
        consensus: false,
        finder: FinderArgs {
            finder: FinderChoice::Agent,
            force_pieces: scale.force_pieces,
            ..FinderArgs::default()
        },
        exclude: Vec::new(),
    }
}

/// The outcome of a result, or a failure that says what the run said.
fn outcome_of(result: Result<InspectOutcome, InspectError>, what: &str) -> InspectOutcome {
    match result {
        Ok(outcome) => outcome,
        Err(err) => panic!("{what}: the run ended with an error instead of an outcome: {err}"),
    }
}

/// What an attempt paid for itself: its usage figure less the stored replies it counts again
/// (`resumed_cost_usd`). Summed over every attempt of a run this is the money the run spent.
fn paid_by(outcome: &InspectOutcome) -> f64 {
    outcome
        .actual_usage
        .as_ref()
        .map_or(0.0, |u| u.cost_usd - u.resumed_cost_usd)
}

fn zip_text(path: &Path, name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn ledger_of(outcome: &InspectOutcome) -> ScanLedger {
    serde_json::from_slice(&std::fs::read(&outcome.ledger_path).unwrap()).unwrap()
}

fn typst_available(test: &str) -> bool {
    camerata_test_env::typst_on_path_for(test)
}

/// Which faults a run plays, besides the 429 burst and the slow batch that every run has.
#[derive(Clone, Copy)]
struct Faults {
    /// The account runs out of credit after this many good finder replies, and gets it back
    /// before the first resume.
    finder_credit_after: Option<usize>,
    /// A credit error on one row of the third batch that holds fix requests.
    batch_credit_row: bool,
    /// Resume once under this spend limit, for the work that remains, which stops the run part
    /// way before the resume that finishes it.
    limit_usd: Option<f64>,
}

/// The environment of a zero-token run.
fn zero_token_env(state: &Path) -> Vec<camerata_test_env::EnvGuard> {
    // A real HTTP client built for a provider would find a closed port, and no keychain is asked.
    vec![
        camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state),
        camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1"),
        camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_SEMGREP", "1"),
        camerata_test_env::EnvGuard::set("CAMERATA_BATCH_POLL_SECS", "1"),
        camerata_test_env::EnvGuard::set(camerata_server::credentials::NO_KEYCHAIN_ENV, "1"),
        camerata_test_env::EnvGuard::set("HTTPS_PROXY", "http://127.0.0.1:9"),
        camerata_test_env::EnvGuard::set("HTTP_PROXY", "http://127.0.0.1:9"),
        camerata_test_env::EnvGuard::set("ALL_PROXY", "http://127.0.0.1:9"),
    ]
}

/// Everything one rehearsal needs: the fixture, the scripts, and the seams.
struct Rig {
    fx: Fixture,
    spend: Arc<Spend>,
    shared: Arc<Shared>,
    fake: Arc<Batches>,
    seam: BatchSeam,
    factory: AgentFactoryOverride,
    export: PathBuf,
    _work: tempfile::TempDir,
    _out: tempfile::TempDir,
}

impl Rig {
    fn new(scale: &Scale, faults: &Faults) -> Self {
        let work = tempfile::tempdir().unwrap();
        let fixture_began = Instant::now();
        let fx = build_fixture(work.path(), scale);
        eprintln!(
            "[rehearsal:{}] fixture: {} repos, {} source files, {} lines, built in {:.1}s",
            scale.name,
            fx.repos.len(),
            fx.admitted_files,
            fx.admitted_lines,
            fixture_began.elapsed().as_secs_f64()
        );
        let spend = Arc::new(Spend::default());
        let shared = Arc::new(Shared {
            spend: spend.clone(),
            hiccups_left: AtomicUsize::new(scale.finder_hiccups),
            hiccups_played: AtomicUsize::new(0),
            pauses: AtomicUsize::new(0),
            turns: AtomicUsize::new(0),
            largest_body: AtomicUsize::new(0),
            sentinel_seen: AtomicBool::new(false),
            credit_after: AtomicUsize::new(faults.finder_credit_after.unwrap_or(usize::MAX)),
            credit_errors: AtomicUsize::new(0),
            problems: Mutex::new(BTreeSet::new()),
            piece_runs: AtomicUsize::new(0),
            joints_runs: AtomicUsize::new(0),
        });
        let fake = Arc::new(Batches::new(spend.clone(), scale));
        fake.credit_armed
            .store(faults.batch_credit_row, Ordering::SeqCst);
        let seam = BatchSeam::new(fake.clone());
        let factory: AgentFactoryOverride = {
            let shared = shared.clone();
            Arc::new(move |meter| {
                Arc::new(Rehearsal {
                    meter,
                    shared: shared.clone(),
                    kept: Mutex::new(HashMap::new()),
                }) as Arc<dyn PieceAgent>
            })
        };
        let out = tempfile::tempdir().unwrap();
        let export = out.path().join("inspect.zip");
        Self {
            fx,
            spend,
            shared,
            fake,
            seam,
            factory,
            export,
            _work: work,
            _out: out,
        }
    }

    fn seams(&self) -> Seams<'_> {
        Seams {
            resume: None,
            llm: Some(&self.seam),
            preflight_sink: &|_| {},
        }
    }

    /// The first run.
    async fn start(&self, scale: &Scale) -> InspectOutcome {
        let result = Box::pin(with_agent_factory_override(
            self.factory.clone(),
            run_inspect_pipeline_with(
                inspect_args(&self.fx, &self.export, scale),
                true,
                &store(),
                self.seams(),
            ),
        ))
        .await;
        outcome_of(result, "the first run")
    }

    /// A resume of the saved run, under `limit` when there is one.
    async fn resume(&self, run_id: &str, limit: Option<f64>) -> InspectOutcome {
        let result = Box::pin(with_agent_factory_override(
            self.factory.clone(),
            resume_inspect(
                run_id,
                &[],
                Some(self.export.clone()),
                limit,
                true,
                &store(),
                self.seams(),
            ),
        ))
        .await;
        outcome_of(result, "a resume")
    }

    /// The credit comes back.
    fn restore_credit(&self) {
        self.shared.credit_after.store(usize::MAX, Ordering::SeqCst);
    }
}

async fn rehearse(scale: Scale, faults: Faults) {
    let began = Instant::now();
    let memory = MemorySampler::start();
    let state = tempfile::tempdir().unwrap();
    let _guards = zero_token_env(state.path());
    let _span = camerata_server::run_health::OutageSpanGuard::set(Duration::ZERO);
    let rig = Rig::new(&scale, &faults);
    let (fx, spend, shared, fake) = (&rig.fx, &rig.spend, &rig.shared, &rig.fake);
    let export = rig.export.clone();

    // ---- Attempt A: the 429 burst and the slow batch, then the credit runs out ----
    let a_began = Instant::now();
    let a = rig.start(&scale).await;
    eprintln!(
        "[rehearsal:{}] attempt A took {:.1}s: outage stop {}, incomplete {:?}",
        scale.name,
        a_began.elapsed().as_secs_f64(),
        a.outage_stopped(),
        a.incomplete_reasons
    );
    let run_id = a.run_id.clone().expect("the run was saved");
    if faults.finder_credit_after.is_some() {
        assert!(
            shared.credit_errors.load(Ordering::SeqCst) > 0,
            "the finder met a credit error"
        );
        assert!(
            a.outage_stopped(),
            "a credit error in the finder stops the run: {:?}",
            a.incomplete_reasons
        );
        assert!(
            !a.incomplete_reasons.is_empty(),
            "the stopped run says it is incomplete"
        );
    }
    let mut paid = paid_by(&a);
    let sends_after_credit_error = shared.credit_errors.load(Ordering::SeqCst);
    let spend_after_a = spend.micro();
    rig.restore_credit();

    // ---- Attempt B: a resume under a spend limit just above what is spent ----
    let mut spend_after_b = spend_after_a;
    let a_finished = a.incomplete_reasons.is_empty() && !a.outage_stopped() && !a.cost_capped();
    if let (Some(limit), false) = (faults.limit_usd, a_finished) {
        let b_began = Instant::now();
        let b = rig.resume(&run_id, Some(limit)).await;
        spend_after_b = spend.micro();
        eprintln!(
            "[rehearsal:{}] attempt B took {:.1}s: spend limit ${limit:.2} for the work that remains, \
             capped {}, spent after A ${:.4}, after B ${:.4}; incomplete {:?}",
            scale.name,
            b_began.elapsed().as_secs_f64(),
            b.cost_capped(),
            spend_after_a as f64 / 1e6,
            spend_after_b as f64 / 1e6,
            b.incomplete_reasons
        );
        eprintln!(
            "[rehearsal:{}] after B the run counted ${:?}, the scripts reported ${:.4}; B stage figures {:?}",
            scale.name,
            b.actual_usage.as_ref().map(|u| u.cost_usd),
            spend_after_b as f64 / 1e6,
            b.stage_telemetry.as_ref().map(|t| t
                .stages
                .iter()
                .map(|r| (format!("{:?}", r.stage), r.usage.calls, r.usage.cost_usd, r.usage.wasted_calls, r.usage.wasted_cost_usd))
                .collect::<Vec<_>>())
        );
        assert!(b.cost_capped(), "the spend limit stopped the resume");
        assert!(
            b.actual_usage.is_some() && !b.incomplete_reasons.is_empty(),
            "the stopped resume says it is incomplete"
        );
        paid += paid_by(&b);
        let spent_in_b = (spend_after_b - spend_after_a) as f64 / 1e6;
        assert!(
            spent_in_b > 0.0,
            "the resume did some of the work before the limit stopped it"
        );
        assert!(
            spent_in_b <= limit + 0.1,
            "the resume spent ${spent_in_b:.2} against a limit of ${limit:.2}"
        );
        assert_eq!(
            shared.credit_errors.load(Ordering::SeqCst),
            sends_after_credit_error,
            "the credit was back: no more credit errors"
        );
    }

    // ---- Attempt C: a resume with room to finish ----
    let c_began = Instant::now();
    let c = if a_finished {
        a
    } else {
        let c = rig.resume(&run_id, Some(1_000_000.0)).await;
        paid += paid_by(&c);
        c
    };
    eprintln!(
        "[rehearsal:{}] attempt C took {:.1}s; spent ${:.4} in all (A ${:.4}, B ${:.4})",
        scale.name,
        c_began.elapsed().as_secs_f64(),
        spend.usd(),
        spend_after_a as f64 / 1e6,
        spend_after_b as f64 / 1e6,
    );

    // ---- What the faults must have shown ----
    assert_eq!(
        fake.burst_played.load(Ordering::SeqCst),
        scale.burst,
        "the 429 burst on a batch submit was played"
    );
    assert!(
        fake.polls
            .lock()
            .unwrap()
            .values()
            .any(|n| *n > scale.slow_polls),
        "a batch stayed in progress for {} polls and then ended",
        scale.slow_polls
    );
    assert!(
        shared.pauses.load(Ordering::SeqCst) >= scale.finder_hiccups,
        "the finder transport waited out its 429s and 529s ({} waits for {} replies)",
        shared.pauses.load(Ordering::SeqCst),
        scale.finder_hiccups
    );

    // ---- What the finished run must show ----
    assert!(!c.outage_stopped() && !c.cost_capped(), "the run finished");
    assert!(
        c.incomplete_reasons.is_empty(),
        "{:?}",
        c.incomplete_reasons
    );
    assert!(
        c.release_gate.starts_with("release gate: passed"),
        "{}",
        c.release_gate
    );
    assert!(export.exists());

    let ledger = ledger_of(&c);
    let coverage = ledger.finder_coverage();
    assert_eq!(
        coverage.len(),
        fx.repos.len(),
        "a coverage block for every repository"
    );

    // Every admitted file reaches a piece, and every piece read what it owed.
    let owed: usize = coverage
        .iter()
        .flat_map(|b| &b.pieces)
        .map(|p| p.files_owed)
        .sum();
    assert_eq!(
        ledger.input().files_scanned,
        fx.admitted_files,
        "the run admitted every source file the fixture holds"
    );
    assert_eq!(
        owed,
        ledger.input().files_scanned,
        "the files the pieces owe are the files the run admitted"
    );
    for block in coverage {
        for piece in &block.pieces {
            assert!(piece.complete(), "{}: {piece:?}", block.repo);
            assert_eq!(
                piece.read_in_full + piece.sent_again,
                piece.files_owed,
                "{piece:?}"
            );
        }
    }
    let core_pieces = coverage
        .iter()
        .find(|b| b.repo == "acme/core")
        .map_or(0, |b| b.pieces.len());
    assert!(
        core_pieces >= scale.core_min_pieces,
        "the large repository split into {core_pieces} pieces"
    );

    // Coverage names every file left out, with its reason, in the ledger and in the export.
    let skipped = &ledger.input().skipped_files;
    for (repo, path, reason) in &fx.left_out {
        assert!(
            skipped
                .iter()
                .any(|s| &s.repo == repo && &s.path == path && s.reason == *reason),
            "{repo} {path} should be named as left out ({reason:?}); named: {skipped:?}"
        );
    }
    assert_eq!(
        ledger.input().skipped_totals.total(),
        skipped.len() + ledger.input().skipped_beyond_cap_total,
        "every skipped file is named or counted as beyond the list"
    );
    let inspection_text = zip_text(&export, "json/inspection.json");
    for (_, path, _) in &fx.left_out {
        assert!(
            inspection_text.contains(path.as_str()),
            "the export's Coverage names {path}"
        );
    }
    assert!(
        !shared.sentinel_seen.load(Ordering::SeqCst),
        "a file outside the repositories was sent to the model through a link"
    );

    // No problem is lost across the stop and the resume.
    let scripted = shared.problems.lock().unwrap().clone();
    assert!(!scripted.is_empty());
    let inspection: Value = serde_json::from_str(&inspection_text).unwrap();
    let rows = inspection["findings"].as_array().expect("findings array");
    let missing: Vec<&(String, String)> = scripted
        .iter()
        .filter(|(path, code)| {
            !rows.iter().any(|r| {
                r["path"].as_str() == Some(path.as_str())
                    && r["snippet"].as_str().map(str::trim) == Some(code.as_str())
            })
        })
        .collect();
    if !missing.is_empty() {
        let kept = std::env::temp_dir().join(format!("rehearsal-{}-inspection.json", scale.name));
        std::fs::write(&kept, &inspection_text).unwrap();
        panic!(
            "{} of {} scripted problems are not in the export (it is kept at {}): {:?}",
            missing.len(),
            scripted.len(),
            kept.display(),
            &missing[..missing.len().min(8)]
        );
    }

    // The ledger's spend is exactly what the scripts reported.
    let usage = c.actual_usage.as_ref().expect("the run has usage");
    eprintln!(
        "[rehearsal:{}] the scripts reported ${:.4} [finder, real time, batch rows] = {:?}; the \
         attempts paid ${:.4} in all; the finished run's own figure is ${:.4} (it counts the \
         stored replies of earlier attempts once and not the paid calls an attempt did not \
         keep); per-stage figures {:?}",
        scale.name,
        spend.usd(),
        spend.by_kind(),
        paid,
        usage.cost_usd,
        c.stage_telemetry
    );
    assert!(
        (paid - spend.usd()).abs() < 1e-6,
        "the attempts paid ${paid} in all and the scripts reported ${}",
        spend.usd()
    );
    assert!(usage.cost_complete);

    // The export opens, and its counts match the run.
    assert_eq!(
        rows.len(),
        c.findings_count,
        "rows in the export and findings in the run"
    );
    let manifest: Value = serde_json::from_str(&zip_text(&export, "manifest.json")).unwrap();
    assert!(
        !manifest["inspection"]["run_incomplete"]
            .as_bool()
            .unwrap_or(false),
        "{manifest}"
    );
    assert_eq!(manifest["inspection"]["cost_cap_reached"], false);
    assert_eq!(
        manifest["inspection"]["counted_lines"]
            .as_u64()
            .map(|n| n as usize),
        Some(fx.admitted_lines),
        "the export counts the lines the fixture holds"
    );
    let archive = zip::ZipArchive::new(std::fs::File::open(&export).unwrap()).unwrap();
    let names: Vec<String> = archive
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.to_string()))
        .collect();
    assert!(names.iter().any(|n| n.ends_with(".pdf")), "{names:?}");
    assert!(
        names.iter().any(|n| n.ends_with("-findings.xlsx")),
        "{names:?}"
    );

    // Printed, not asserted.
    let peak = memory.finish();
    let by_stage: BTreeMap<String, usize> = {
        let mut m = BTreeMap::new();
        for (kind, _) in fake.accepted.lock().unwrap().iter() {
            *m.entry(format!("{kind:?}")).or_insert(0) += 1;
        }
        m
    };
    eprintln!(
        "[rehearsal:{}] DONE in {:.1}s wall time, peak resident memory {:.0} MiB",
        scale.name,
        began.elapsed().as_secs_f64(),
        peak as f64 / 1024.0
    );
    eprintln!(
        "[rehearsal:{}] {} finder pieces, {} joints calls, {} finder turns (largest request {} KiB), \
         {} waits, {} batches accepted, items by kind {by_stage:?}, {} findings, spend ${:.2}",
        scale.name,
        shared.piece_runs.load(Ordering::SeqCst),
        shared.joints_runs.load(Ordering::SeqCst),
        shared.turns.load(Ordering::SeqCst),
        shared.largest_body.load(Ordering::SeqCst) / 1024,
        shared.pauses.load(Ordering::SeqCst),
        fake.submits_ok.load(Ordering::SeqCst),
        c.findings_count,
        spend.usd()
    );
}

/// A credit error on one row of a batch stops the run and the row is not sent again; a resume,
/// with the credit back, sends only that row again and finishes the run.
async fn credit_row_then_resume(scale: Scale) {
    let state = tempfile::tempdir().unwrap();
    let _guards = zero_token_env(state.path());
    let _span = camerata_server::run_health::OutageSpanGuard::set(Duration::ZERO);
    let faults = Faults {
        finder_credit_after: None,
        batch_credit_row: true,
        limit_usd: None,
    };
    let rig = Rig::new(&scale, &faults);
    let a = rig.start(&scale).await;
    let run_id = a.run_id.clone().expect("the run was saved");
    assert!(
        !rig.fake.credit_armed.load(Ordering::SeqCst),
        "the credit error row was played"
    );
    assert!(
        a.outage_stopped(),
        "a credit error on a batch row stops the run: {:?}",
        a.incomplete_reasons
    );
    let credit_id = rig.fake.credit_custom_id.lock().unwrap().clone().unwrap();
    assert_eq!(
        rig.fake.times_accepted(&credit_id),
        1,
        "the row that failed for credit was not sent again in the run that stopped"
    );
    // The credit is back. The resume must send the row again and finish.
    let c = rig.resume(&run_id, None).await;
    assert!(
        c.incomplete_reasons.is_empty() && !c.outage_stopped(),
        "the resume finishes the run; it said: {:?}",
        c.incomplete_reasons
    );
    assert!(
        rig.fake.times_accepted(&credit_id) >= 2,
        "the row that failed for credit was sent again by the resume"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_small_rehearsal_of_the_whole_scan_runs_to_a_gated_export() {
    if !typst_available("a_small_rehearsal_of_the_whole_scan") {
        return;
    }
    on_a_big_stack(rehearse(SMALL, SMALL_FAULTS));
}

#[test]
#[serial_test::serial(process_env)]
#[ignore = "full size: about 200K lines and three attempts, minutes of wall time"]
fn the_full_size_rehearsal_of_the_whole_scan_runs_to_a_gated_export() {
    if !typst_available("the_full_size_rehearsal_of_the_whole_scan") {
        return;
    }
    on_a_big_stack(rehearse(FULL, FULL_FAULTS));
}

#[test]
#[serial_test::serial(process_env)]
fn a_resume_after_a_credit_row_in_a_batch_sends_the_row_again_and_finishes() {
    if !typst_available("a_resume_after_a_credit_row_in_a_batch") {
        return;
    }
    on_a_big_stack(credit_row_then_resume(SMALL));
}

// ---------------------------------------------------------------------------------------------
// A tracked path that is not UTF-8
// ---------------------------------------------------------------------------------------------

/// The walk reads the files git tracks and the walk skipped (a committed file under a
/// `.gitignore` pattern) from `git ls-files -z`. One tracked path that is not UTF-8 must not make
/// that whole list unusable: the other tracked files are read, and the bad path is named as not
/// read. This machine's file system cannot hold a non-UTF-8 name, so the path is put into the
/// index only.
#[test]
fn a_tracked_path_that_is_not_utf8_does_not_hide_the_other_tracked_files() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "rehearsal@camerata.local"]);
    git(root, &["config", "user.name", "Rehearsal"]);
    std::fs::write(root.join(".gitignore"), "hidden/\n").unwrap();
    std::fs::create_dir_all(root.join("hidden")).unwrap();
    std::fs::write(root.join("hidden/kept.py"), "def kept():\n    return 1\n").unwrap();
    std::fs::write(root.join("main.py"), "def main():\n    return 2\n").unwrap();
    git(root, &["add", "-f", "hidden/kept.py"]);
    git(root, &["add", "."]);
    // A blob, then an index entry whose path bytes are not UTF-8.
    let blob = {
        use std::io::Write;
        let mut child = std::process::Command::new("git")
            .current_dir(root)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"def legacy():\n    return 3\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let bad_name = std::ffi::OsStr::from_bytes(b"legacy_\xff.py");
    // The argument is built as bytes: the path is not text.
    let mut arg: Vec<u8> = format!("100644,{blob},").into_bytes();
    arg.extend_from_slice(bad_name.as_bytes());
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(["update-index", "--add", "--cacheinfo"])
        .arg(std::ffi::OsStr::from_bytes(&arg))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(root, &["commit", "-q", "-m", "fixture"]);

    let extracted = camerata_server::onboard::files::read_local_repo_files(
        root,
        &camerata_server::onboard::scope::RepoScope::none(),
    )
    .unwrap();
    let read: Vec<&str> = extracted.files.iter().map(|(p, _)| p.as_str()).collect();
    assert!(read.contains(&"main.py"), "{read:?}");
    assert!(
        read.contains(&"hidden/kept.py"),
        "the tracked file under a .gitignore pattern is read: {read:?}"
    );
    assert_eq!(
        extracted.names_not_text,
        ["legacy_\\xFF.py"],
        "the bad path is named as not read, with its invalid byte as a hex escape"
    );
}
