//! The model-input guard (the platform guard's twin): nothing builds or sends a model request in
//! this workspace except through the chokepoint (`camerata_secrets::model_input`, reached through
//! `camerata_llm::model_input`).
//!
//! Redaction used to be applied site by site, which is convention. This test makes it mechanical:
//! it reads the source of every crate and fails when
//!
//! - a file names a model endpoint (the Messages, Batches or chat-completions URLs) or spawns a
//!   model CLI (`claude`, or the generic agent driver's program) and is not on the list of known
//!   send sites below: a NEW way to send a request must be added to the list on purpose, with the
//!   function that guards it; or
//! - a known send site's request builder no longer calls the chokepoint.
//!
//! The two checks are pure functions over `(path, source)` so the tests at the bottom can prove
//! the guard catches a planted bypass.

use std::path::{Path, PathBuf};

/// Text that marks a file as one that builds or sends a model request.
const SEND_MARKERS: &[&str] = &[
    "api.anthropic.com/v1/messages",
    "openrouter.ai/api/v1/chat/completions",
    "api.openai.com",
    "generativelanguage.googleapis.com",
    "api.mistral.ai",
    "localhost:11434",
    "Command::new(\"claude\")",
    "Command::new(&self.program)",
    // The paths of the model APIs, whatever host a config or a test points them at.
    "/v1/messages",
    "/chat/completions",
    "v1beta/models",
];

/// Whether `code` (a file's source without its comment lines) holds a send marker, and which.
///
/// Beyond the fixed texts: a string literal naming a model provider's host (`anthropic.com`,
/// `openrouter.ai`), and a `Command::new` of anything but a literal in a file that also names the
/// `claude` CLI or its `-p` flag (the program is then chosen at run time, and it may be a model
/// CLI under another name).
fn marker_in(code: &str) -> Option<String> {
    if let Some(marker) = SEND_MARKERS.iter().find(|m| code.contains(**m)) {
        return Some((*marker).to_string());
    }
    for host in ["anthropic.com", "openrouter.ai"] {
        if code
            .lines()
            .any(|line| line.contains('"') && line.contains(host))
        {
            return Some(format!("a string literal naming {host}"));
        }
    }
    let names_the_cli = code.contains("\"claude\"") || code.contains("\"-p\"");
    if names_the_cli {
        for line in code.lines() {
            if let Some(at) = line.find("Command::new(") {
                let arg = line[at + "Command::new(".len()..].trim_start();
                if !arg.starts_with('"') {
                    return Some(
                        "Command::new of a name chosen at run time, beside `claude`".into(),
                    );
                }
            }
        }
    }
    None
}

/// A known send site: the file, and the functions in it that build what is sent, each with the
/// text its body must contain (the call into the chokepoint).
struct Site {
    file: &'static str,
    builders: &'static [(&'static str, &'static str)],
}

const SITES: &[Site] = &[
    // The transports: every wire builder reads the request through `model_input::guarded`.
    Site {
        file: "crates/llm/src/llm.rs",
        builders: &[
            ("messages_request_body", "model_input::guarded("),
            ("count_tokens_body", "model_input::guarded("),
            ("build_batch_item", "model_input::guarded("),
            ("build_openrouter_chat_body", "model_input::guarded("),
            ("build_cli_command", "model_input::guarded("),
            ("batch_submit_body", "model_input::redact_json_for_model("),
        ],
    },
    // The agentic API loop speaks to the provider directly (it sends tool schemas).
    Site {
        file: "crates/server/src/api_agent_driver.rs",
        builders: &[
            ("build_anthropic_request_body", "redact_conversation("),
            ("build_openrouter_agent_body", "redact_conversation("),
            ("redact_conversation", "model_input::redact_json_for_model("),
        ],
    },
    // The governed agent drivers hand the task to a CLI as an argument.
    Site {
        file: "crates/agent/src/lib.rs",
        builders: &[("build_args", "model_input::redact_for_model(")],
    },
    Site {
        file: "crates/agent/src/generic.rs",
        builders: &[("build_args", "model_input::redact_for_model(")],
    },
    // The intake lead engineer and refinement reviewer.
    Site {
        file: "crates/intake/src/engine.rs",
        builders: &[("build_command", "model_input::redact_for_model(")],
    },
    Site {
        file: "crates/intake/src/review.rs",
        builders: &[("build_command", "model_input::redact_for_model(")],
    },
];

/// Files that name a marker and send nothing: the exact file, why, and the texts that must NOT
/// appear in it (so the exemption cannot quietly grow a sender). No directory is exempt: a new
/// module next to these is scanned.
const NOT_SENDERS: &[(&str, &str, &[&str])] = &[
    (
        "crates/api-types/src/claude_cli.rs",
        "builds a `claude` command in its one unit test, to check the API key is removed from the \
         child's environment; the command is never spawned",
        &[".spawn(", ".output(", ".status("],
    ),
    (
        "crates/llm/src/llm/stream_tests.rs",
        "the transports' test module that mocks the Messages endpoint with a local server",
        &["reqwest::", "Command::new"],
    ),
    (
        "crates/llm/src/llm/model_input_tests.rs",
        "the chokepoint's own tests: they read the wire bodies and argv the builders make",
        &["reqwest::", ".spawn(", ".output("],
    ),
    (
        "crates/llm/src/llm/temperature_tests.rs",
        "the transports' test module for the temperature field of the wire body",
        &["reqwest::", "Command::new"],
    ),
    (
        "crates/server/src/cockpit_resume_tests.rs",
        "test module: runs its own fake `claude` script once to warm it, before the server under \
         test calls it through the transports",
        &["reqwest::", "api."],
    ),
    (
        "crates/server/src/model_input_chat_tests.rs",
        "test module: runs its own recording fake `claude` script once to warm it; the request \
         itself goes through the chat handler and the transports",
        &["reqwest::", "api."],
    ),
    (
        "crates/llm/src/model_registry.rs",
        "lists models (GET /v1/models, OpenRouter's model and provider catalogues); it sends no \
         prompt",
        &[".post(", "Command::new"],
    ),
];

fn not_a_sender(file: &str) -> Option<&'static [&'static str]> {
    NOT_SENDERS
        .iter()
        .find(|(path, _, _)| *path == file)
        .map(|(_, _, forbidden)| *forbidden)
}

/// The body of `fn name` in `source` (from its opening brace to the matching close), if there is
/// such a function. Braces are counted; the builders under test hold no unbalanced brace in a
/// string.
fn body_of<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("fn {name}");
    let mut from = 0;
    while let Some(at) = source[from..].find(&needle) {
        let start = from + at;
        from = start + needle.len();
        let after = &source[from..];
        // `fn name` must be the whole identifier: `fn name(` or `fn name<`.
        if !after.starts_with('(') && !after.starts_with('<') {
            continue;
        }
        let open = source[start..].find('{')? + start;
        let mut depth = 0usize;
        for (i, c) in source[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&source[open..open + i + 1]);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Every way `files` (path relative to the workspace root, source text) breaks the rule.
fn violations(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, source) in files {
        let site = SITES.iter().find(|s| s.file == path);
        // A comment that mentions an endpoint sends nothing.
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let marker = marker_in(&code);
        if let (Some(forbidden), true) = (not_a_sender(path), site.is_none()) {
            for text in forbidden {
                if code.contains(text) {
                    out.push(format!(
                        "{path} is listed in NOT_SENDERS but now holds `{text}`: it may send; \
                         move it to SITES with the function that guards it"
                    ));
                }
            }
        }
        match (site, marker) {
            (None, Some(marker)) if not_a_sender(path).is_none() => out.push(format!(
                "{path} names `{marker}` but is not a known model send site: route the request \
                 through camerata_llm::model_input (or camerata_secrets::model_input) and add the \
                 file to SITES with the function that guards it"
            )),
            (Some(site), _) => {
                for (builder, must_contain) in site.builders {
                    match body_of(source, builder) {
                        None => out.push(format!("{path}: `fn {builder}` is gone; update SITES")),
                        Some(body) if !body.contains(must_contain) => out.push(format!(
                            "{path}: `fn {builder}` builds what is sent to a model and does not \
                             call `{must_contain}`"
                        )),
                        Some(_) => {}
                    }
                }
            }
            _ => {}
        }
    }
    // A listed site that no longer exists is a stale list, not a pass.
    for site in SITES {
        if !files.iter().any(|(path, _)| path == site.file) {
            out.push(format!(
                "{} is listed in SITES but was not found",
                site.file
            ));
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The source of every crate (`crates/*/src/**.rs`) as (workspace-relative path, text).
fn workspace_sources() -> Vec<(String, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory")
        .to_path_buf();
    let root = crates.parent().expect("the workspace root").to_path_buf();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).expect("read crates/").flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let rel = path
                .strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((rel, text))
        })
        .collect()
}

#[test]
fn no_crate_builds_or_sends_a_model_request_except_through_the_chokepoint() {
    let files = workspace_sources();
    assert!(
        files.len() > 200,
        "the scan must actually see the workspace ({} files)",
        files.len()
    );
    let found = violations(&files);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The guard works: a send site nobody listed is caught.
#[test]
fn a_planted_new_send_site_is_caught() {
    let mut files = workspace_sources();
    files.push((
        "crates/somewhere/src/new_client.rs".to_string(),
        "async fn go() { let c = tokio::process::Command::new(\"claude\"); }".to_string(),
    ));
    let found = violations(&files);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("new_client.rs"), "{found:?}");

    let mut files = workspace_sources();
    files.push((
        "crates/somewhere/src/new_http.rs".to_string(),
        "const URL: &str = \"https://api.anthropic.com/v1/messages\";".to_string(),
    ));
    assert_eq!(violations(&files).len(), 1);
}

/// The guard works: a known builder that stops calling the chokepoint is caught.
#[test]
fn a_known_builder_that_skips_the_chokepoint_is_caught() {
    let files: Vec<(String, String)> = workspace_sources()
        .into_iter()
        .map(|(path, text)| {
            if path == "crates/llm/src/llm.rs" {
                // The bypass: the builder reads the request directly.
                (
                    path,
                    text.replace(
                        "crate::model_input::guarded(req)",
                        "std::borrow::Cow::Borrowed(req)",
                    ),
                )
            } else {
                (path, text)
            }
        })
        .collect();
    let found = violations(&files);
    assert!(
        found.iter().any(|v| v.contains("messages_request_body")),
        "{found:?}"
    );
    assert!(
        found.iter().any(|v| v.contains("build_cli_command")),
        "{found:?}"
    );
}

/// Each new marker is caught in a file nobody listed.
#[test]
fn every_kind_of_send_marker_is_caught_in_an_unlisted_file() {
    let planted: &[(&str, &str)] = &[
        ("an Anthropic path", "const P: &str = \"/v1/messages\";"),
        ("a chat completions path", "let url = format!(\"{base}/chat/completions\");"),
        ("a Gemini path", "let url = \"models/x:generate\"; let v = \"v1beta/models\";"),
        ("an Anthropic host", "let host = \"api.anthropic.com\";"),
        ("an OpenRouter host", "let host = \"https://openrouter.ai/api\";"),
        (
            "a program chosen at run time beside the claude CLI",
            "const BIN: &str = \"claude\";\nfn go(bin: &str) { let c = tokio::process::Command::new(bin); }",
        ),
        (
            "the claude -p flag with a program chosen at run time",
            "fn go(p: &str) { let c = Command::new(p).arg(\"-p\"); }",
        ),
    ];
    for (what, source) in planted {
        let mut files = workspace_sources();
        files.push((
            "crates/somewhere/src/new_sender.rs".to_string(),
            (*source).to_string(),
        ));
        let found = violations(&files);
        assert_eq!(found.len(), 1, "{what}: {found:?}");
        assert!(found[0].contains("new_sender.rs"), "{what}: {found:?}");
    }
}

/// No directory is exempt: a new module beside the transports' test modules is scanned.
#[test]
fn a_new_module_in_the_transport_directory_is_scanned() {
    let mut files = workspace_sources();
    files.push((
        "crates/llm/src/llm/second_transport.rs".to_string(),
        "const URL: &str = \"https://api.anthropic.com/v1/messages\";".to_string(),
    ));
    let found = violations(&files);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("second_transport.rs"), "{found:?}");
}

/// A file that was cleared as sending nothing is held to it.
#[test]
fn an_exempt_file_that_starts_to_send_is_caught() {
    let files: Vec<(String, String)> = workspace_sources()
        .into_iter()
        .map(|(path, text)| {
            if path == "crates/llm/src/model_registry.rs" {
                (path, format!("{text}\nfn leak(c: &reqwest::Client) {{ c.post(\"https://example.test\"); }}"))
            } else {
                (path, text)
            }
        })
        .collect();
    let found = violations(&files);
    assert!(
        found
            .iter()
            .any(|v| v.contains("model_registry.rs") && v.contains(".post(")),
        "{found:?}"
    );
}
