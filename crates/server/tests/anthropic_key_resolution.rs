#![allow(clippy::unwrap_used)]
//! The Anthropic API key saved in the app is the key every model call uses, with no restart.
//!
//! One resolver (`credentials::anthropic_api_key`: credential store first, `ANTHROPIC_API_KEY`
//! env only as the fallback) feeds every `Llm` the server builds for an audit or rescan
//! (`onboard::build_llm`). HERMETIC: in-memory credential store, no Keychain, no network.

use std::fs;
use std::path::{Path, PathBuf};

use camerata_server::credentials::{
    CredentialStore, MemoryCredentialStore, ANTHROPIC_API_KEY as ANTHROPIC_CRED,
};
use camerata_server::llm::Backend;
use camerata_server::onboard::build_llm;
use camerata_test_env::EnvGuard;

fn key_of(store: &MemoryCredentialStore) -> Option<String> {
    build_llm(store, Backend::Api, None)
        .api_key()
        .map(str::to_string)
}

/// A key saved AFTER the "server" (a long-lived store handle) already exists reaches the very
/// next `Llm`: nothing is captured at startup, so no restart is needed.
#[test]
#[serial_test::serial(process_env)]
fn key_saved_after_start_reaches_the_next_llm_with_env_unset() {
    let _env = EnvGuard::remove("ANTHROPIC_API_KEY");
    let store = MemoryCredentialStore::new();
    assert_eq!(key_of(&store), None, "no key anywhere yet");
    store.set(ANTHROPIC_CRED, "sk-ant-saved-later").unwrap();
    assert_eq!(key_of(&store).as_deref(), Some("sk-ant-saved-later"));
}

#[test]
#[serial_test::serial(process_env)]
fn replaced_key_reaches_the_next_llm() {
    let _env = EnvGuard::remove("ANTHROPIC_API_KEY");
    let store = MemoryCredentialStore::new();
    store.set(ANTHROPIC_CRED, "sk-ant-old").unwrap();
    assert_eq!(key_of(&store).as_deref(), Some("sk-ant-old"));
    store.set(ANTHROPIC_CRED, "sk-ant-new").unwrap();
    assert_eq!(key_of(&store).as_deref(), Some("sk-ant-new"));
}

#[test]
#[serial_test::serial(process_env)]
fn empty_store_uses_env_and_store_wins_when_both_are_set() {
    let store = MemoryCredentialStore::new();
    let _env = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
    assert_eq!(key_of(&store).as_deref(), Some("sk-ant-env"));
    store.set(ANTHROPIC_CRED, "sk-ant-store").unwrap();
    assert_eq!(
        key_of(&store).as_deref(),
        Some("sk-ant-store"),
        "the app-saved key must win over the env fallback"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn blank_store_value_is_absent() {
    let store = MemoryCredentialStore::new();
    store.set(ANTHROPIC_CRED, "   ").unwrap();
    let _env = EnvGuard::remove("ANTHROPIC_API_KEY");
    assert_eq!(key_of(&store), None);
    let _env2 = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
    assert_eq!(
        key_of(&store).as_deref(),
        Some("sk-ant-env"),
        "a blank saved value must not shadow the env fallback"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_cli_backend_llm_is_still_built_without_a_key() {
    let _env = EnvGuard::remove("ANTHROPIC_API_KEY");
    let llm = build_llm(&MemoryCredentialStore::new(), Backend::Cli, None);
    assert_eq!(llm.api_key(), None);
}

// ── regression guard: no production code reads the key from env, or builds an api Llm from env ──

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Lexer state carried across lines, so braces inside strings, raw strings, char literals and
/// comments are never mistaken for structure.
#[derive(Clone, Copy, PartialEq)]
enum Lex {
    Code,
    Str,
    Raw(usize),
    Block(usize),
}

/// The structural characters (`{`, `}`, `;`) of one line that sit in real code, updating the
/// lexer state for the next line.
fn structural(line: &str, st: &mut Lex) -> Vec<char> {
    let b: Vec<char> = line.chars().collect();
    let at = |i: usize| b.get(i).copied().unwrap_or('\0');
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match *st {
            Lex::Code => {
                let c = b[i];
                if c == '/' && at(i + 1) == '/' {
                    break;
                }
                if c == '/' && at(i + 1) == '*' {
                    *st = Lex::Block(1);
                    i += 2;
                    continue;
                }
                if c == '"' {
                    *st = Lex::Str;
                    i += 1;
                    continue;
                }
                if c == 'r' {
                    let mut h = 0;
                    while at(i + 1 + h) == '#' {
                        h += 1;
                    }
                    if at(i + 1 + h) == '"' {
                        *st = Lex::Raw(h);
                        i += h + 2;
                        continue;
                    }
                }
                if c == '\'' {
                    if at(i + 1) == '\\' {
                        // Escaped char literal: skip to the closing quote.
                        let mut j = i + 2;
                        while j < b.len() && b[j] != '\'' {
                            j += 1;
                        }
                        i = j + 1;
                    } else if at(i + 2) == '\'' {
                        i += 3;
                    } else {
                        i += 1; // a lifetime
                    }
                    continue;
                }
                if matches!(c, '{' | '}' | ';') {
                    out.push(c);
                }
                i += 1;
            }
            Lex::Str => {
                match b[i] {
                    '\\' => i += 1,
                    '"' => *st = Lex::Code,
                    _ => {}
                }
                i += 1;
            }
            Lex::Raw(h) => {
                if b[i] == '"' && (1..=h).all(|k| at(i + k) == '#') {
                    *st = Lex::Code;
                    i += h;
                }
                i += 1;
            }
            Lex::Block(d) => {
                if b[i] == '/' && at(i + 1) == '*' {
                    *st = Lex::Block(d + 1);
                    i += 1;
                } else if b[i] == '*' && at(i + 1) == '/' {
                    *st = if d == 1 { Lex::Code } else { Lex::Block(d - 1) };
                    i += 1;
                }
                i += 1;
            }
        }
    }
    out
}

/// Production source of one file: every line EXCEPT comment lines and `#[cfg(test)]` items.
///
/// A column-0 `#[cfg(test)]` removes only the ITEM it annotates (an inline `mod tests { .. }`
/// through its matching `}`, a `mod tests;`, a `use ..;`, a helper `fn`), never the rest of the
/// file. (An earlier version stopped at the first `#[cfg(test)]`, which hid most of the
/// production code in files that declare a test module near the top.) Braces are matched with a
/// small lexer that ignores strings, raw strings, char literals and comments, so test fixtures
/// holding unbalanced braces cannot end the skip early. Test-only files (`*tests.rs`, anything
/// under a `tests/` dir) are skipped by the caller.
fn production_lines(path: &Path) -> Vec<(usize, String)> {
    let text = fs::read_to_string(path).unwrap();
    let mut out = Vec::new();
    let mut st = Lex::Code;
    // While skipping a test item: (brace depth, whether its body has opened).
    let mut skip: Option<(i32, bool)> = None;
    for (i, line) in text.lines().enumerate() {
        let before = st;
        let events = structural(line, &mut st);
        if let Some((depth, opened)) = skip.as_mut() {
            for ev in events {
                match ev {
                    '{' => {
                        *depth += 1;
                        *opened = true;
                    }
                    '}' => {
                        *depth -= 1;
                        if *depth <= 0 && *opened {
                            skip = None;
                            break;
                        }
                    }
                    _ => {
                        if *depth == 0 && !*opened {
                            skip = None;
                            break;
                        }
                    }
                }
            }
            continue;
        }
        if before == Lex::Code && line.starts_with("#[cfg(test)]") {
            skip = Some((0, false));
            continue;
        }
        if before == Lex::Code && line.trim_start().starts_with("//") {
            continue;
        }
        out.push((i + 1, line.to_string()));
    }
    out
}

fn production_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in fs::read_dir(&crates).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    files
        .into_iter()
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            !(name.ends_with("tests.rs")
                || name == "tests.rs"
                || p.components().any(|c| c.as_os_str() == "tests"))
        })
        .collect()
}

/// The env fallbacks of the three credentials the app saves (Anthropic key, OpenRouter key,
/// GitHub token). Only the resolver in `camerata-llm`'s `credentials.rs` (via its `*_ENV`
/// constants) may name them as string literals; any other non-test code that does is reading
/// env directly and skips the app-saved credential.
const CREDENTIAL_ENV_VARS: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "OPENROUTER_API_KEY",
    "CAMERATA_GITHUB_TOKEN",
];

/// Lines the scan MUST have seen. If a change to `production_lines` (or a moved file) makes it
/// stop short again, the scan would pass vacuously over code it never read; these canaries turn
/// that into a loud failure. They span the places that matter: the CLI entry point, the audit
/// `Llm` constructor, the agent driver builders, the resolver, and a late function in the
/// largest file.
const CANARIES: [(&str, &str); 7] = [
    ("cli/src/inspect_cmd.rs", "pub async fn run_inspect("),
    ("server/src/onboard.rs", "pub fn build_llm("),
    ("server/src/api_agent_driver.rs", "fn build_claude_driver("),
    (
        "server/src/api_agent_driver.rs",
        "fn resolve_claude_backend(",
    ),
    ("llm/src/credentials.rs", "pub fn anthropic_api_key("),
    ("server/src/lib.rs", "fn check_github_release("),
    ("server/src/lib.rs", "async fn delete_credential("),
];

fn assert_canaries_seen(scanned: &[(PathBuf, Vec<(usize, String)>)]) {
    for (file, needle) in CANARIES {
        let seen = scanned
            .iter()
            .filter(|(p, _)| p.ends_with(file))
            .any(|(_, lines)| lines.iter().any(|(_, l)| l.contains(needle)));
        assert!(
            seen,
            "the scan never saw `{needle}` in {file}: it is truncating files or the code moved"
        );
    }
}

fn scan_all() -> Vec<(PathBuf, Vec<(usize, String)>)> {
    let scanned: Vec<_> = production_sources()
        .into_iter()
        .map(|p| {
            let lines = production_lines(&p);
            (p, lines)
        })
        .collect();
    assert_canaries_seen(&scanned);
    scanned
}

#[test]
fn no_production_code_reads_a_saved_credential_from_env_directly() {
    let mut offenders = Vec::new();
    for (path, lines) in scan_all() {
        // The resolver itself is the one place allowed to name them.
        if path.ends_with("llm/src/credentials.rs") {
            continue;
        }
        for (n, line) in lines {
            for var in CREDENTIAL_ENV_VARS {
                if line.contains(&format!("\"{var}\"")) {
                    offenders.push(format!("{}:{n}: {}", path.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "resolve credentials through `credentials::resolve_credential` (or its \
         `anthropic_api_key` / `openrouter_api_key` / `github_token` wrappers), never from env \
         directly:\n{}",
        offenders.join("\n")
    );
}

/// The env-reading `Llm` constructors are gone; an api-capable `Llm` is built with an explicit,
/// resolved key (`Llm::for_backend*`), and every production call site resolves it via
/// `anthropic_api_key` within a few lines.
#[test]
fn every_production_llm_build_gets_its_key_from_the_resolver() {
    let mut offenders = Vec::new();
    for (path, lines) in scan_all() {
        for (idx, (n, line)) in lines.iter().enumerate() {
            if line.contains("from_env_with_backend") {
                offenders.push(format!(
                    "{}:{n}: env-reading constructor: {}",
                    path.display(),
                    line.trim()
                ));
            }
            let is_call = (line.contains("Llm::for_backend(")
                || line.contains("Llm::for_backend_and_ledger("))
                && !line.contains("fn ");
            if is_call {
                let lo = idx.saturating_sub(8);
                let hi = (idx + 6).min(lines.len());
                let near_resolver = lines[lo..hi]
                    .iter()
                    .any(|(_, l)| l.contains("anthropic_api_key("));
                if !near_resolver {
                    offenders.push(format!(
                        "{}:{n}: Llm built without the shared key resolver nearby: {}",
                        path.display(),
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}
