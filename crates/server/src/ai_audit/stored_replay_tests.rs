//! Retry fixes F3, item 6, against the real stored replies of the two efficiency runs.
//!
//! Replays every stored fix reply of the Haiku and Sonnet runs against a repository rebuilt from
//! what the same run stores hold of it: the paths its scan calls framed and the code each scan
//! reply quoted. The real inventions (`billing_accounts_user_id_fkey`, the proposed
//! `app/api/charge/route.ts`) are in no such repository and must still be flagged.
//!
//! Ignored in the normal run: it reads a folder outside the repository. Set
//! `CAMERATA_STORED_RUNS` to the folder holding the `state-*` directories and run it with
//! `--ignored --nocapture`.

use super::*;

const INVENTIONS: [&str; 2] = ["billing_accounts_user_id_fkey", "app/api/charge/route.ts"];

fn read_calls(dir: &str, state: &str) -> Vec<serde_json::Value> {
    let runs = std::path::Path::new(dir).join(state).join("camerata/runs");
    let mut out = Vec::new();
    for run in std::fs::read_dir(runs).into_iter().flatten().flatten() {
        for call in std::fs::read_dir(run.path().join("calls"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Ok(bytes) = std::fs::read(call.path()) {
                if let Ok(v) = serde_json::from_slice(&bytes) {
                    out.push(v);
                }
            }
        }
    }
    out
}

fn reply_texts(call: &serde_json::Value) -> Vec<String> {
    call["entries"]
        .as_object()
        .into_iter()
        .flat_map(|e| e.values())
        .filter_map(|e| e["response"]["text"].as_str().map(str::to_string))
        .collect()
}

/// The repository as a run store shows it: path -> the code and text its scan replies quoted.
fn repository_of(calls: &[serde_json::Value]) -> Vec<(String, String)> {
    let mut quoted: std::collections::BTreeMap<String, String> = Default::default();
    for call in calls.iter().filter(|c| c["kind"] == "scan") {
        for p in call["files"].as_array().into_iter().flatten() {
            quoted
                .entry(p.as_str().unwrap_or_default().to_string())
                .or_default();
        }
        for text in reply_texts(call) {
            let Some(json) = extract_json_object(&text) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
                continue;
            };
            for f in v["findings"].as_array().into_iter().flatten() {
                let slot = quoted
                    .entry(f["path"].as_str().unwrap_or_default().to_string())
                    .or_default();
                for key in ["code", "title", "detail"] {
                    slot.push_str(f[key].as_str().unwrap_or_default());
                    slot.push('\n');
                }
            }
        }
    }
    quoted.into_iter().collect()
}

/// The fix texts of a stored fix reply: the `fix` of each entry, or the whole reply when the
/// model answered in prose.
fn fix_texts(reply: &str) -> Vec<String> {
    match extract_json_object(reply).and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
    {
        Some(v) => v["fixes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["fix"].as_str().map(str::to_string))
            .collect(),
        None => vec![reply.to_string()],
    }
}

/// Every name the check asks for that the repository does not hold, in order.
fn unknown_names(fix: &str, repo: &RepoNames<'_>) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = fix.to_string();
    while let Some(bad) = find_ungrounded_identifier_in(&rest, "", Some(repo)) {
        names.push(bad.clone());
        rest = rest.replace(&bad, " ");
    }
    names
}

#[test]
#[ignore = "reads the stored run stores of the two efficiency runs; set CAMERATA_STORED_RUNS"]
fn the_stored_fix_replies_keep_their_real_inventions_out() {
    let Ok(dir) = std::env::var("CAMERATA_STORED_RUNS") else {
        return;
    };
    let mut seen = std::collections::BTreeSet::new();
    for state in ["state-sonnet-all-on", "state-haiku-check-all-on"] {
        let calls = read_calls(&dir, state);
        let files = repository_of(&calls);
        let repo = RepoNames::new(&files);
        let (mut fixes, mut known_only) = (0usize, 0usize);
        let mut elsewhere = std::collections::BTreeSet::new();
        for call in calls.iter().filter(|c| c["kind"] == "fix") {
            for reply in reply_texts(call) {
                for fix in fix_texts(&reply) {
                    fixes += 1;
                    let unknown = unknown_names(&fix, &repo);
                    if unknown.is_empty() {
                        known_only += 1;
                    }
                    for invented in INVENTIONS {
                        if fix.contains(invented) {
                            assert!(
                                unknown
                                    .iter()
                                    .any(|n| n == invented || invented.contains(n.as_str())),
                                "{invented} must still be flagged, got {unknown:?} for: {fix}"
                            );
                            seen.insert(invented);
                        }
                    }
                    elsewhere.extend(unknown);
                }
            }
        }
        println!(
            "{state}: {fixes} stored fixes over {} files; {known_only} name only things the \
             stored scan replies show; {} distinct names are in none: {:?}",
            files.len(),
            elsewhere.len(),
            elsewhere.iter().take(60).collect::<Vec<_>>()
        );
    }
    println!("inventions found in the stored fixes and still flagged: {seen:?}");
}

// ── against the real repository ─────────────────────────────────────────────────────────────

/// The names the F1 review said the window rejected although the repository holds them.
const REAL_NAMES_OUTSIDE_THE_WINDOW: [&str; 4] =
    ["actor_id", "bucket_id", "tsconfig.json", ".env.example"];

/// The file set a run scanned, read from the real repository at `root`: the union of the paths
/// its stored calls frame, each with its content on disk (a path no longer there is skipped).
fn real_repository(root: &str, calls: &[serde_json::Value]) -> Vec<(String, String)> {
    let mut paths = std::collections::BTreeSet::new();
    for call in calls {
        for p in call["files"].as_array().into_iter().flatten() {
            if let Some(p) = p.as_str() {
                paths.insert(p.to_string());
            }
        }
    }
    paths
        .into_iter()
        .filter_map(|p| {
            std::fs::read(std::path::Path::new(root).join(&p))
                .ok()
                .map(|bytes| (p, String::from_utf8_lossy(&bytes).into_owned()))
        })
        .collect()
}

/// The findings a run's scan replies raised (every parseable reply), as the fix stage saw them.
fn findings_of(calls: &[serde_json::Value]) -> Vec<Finding> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for call in calls.iter().filter(|c| c["kind"] == "scan") {
        for text in reply_texts(call) {
            let Some(json) = extract_json_object(&text) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
                continue;
            };
            for f in v["findings"].as_array().into_iter().flatten() {
                let path = f["path"].as_str().unwrap_or_default().to_string();
                let line = f["line"].as_u64().unwrap_or(0) as usize;
                let rule = f["rule"].as_str().unwrap_or_default().to_string();
                if !seen.insert((path.clone(), line, rule.clone())) {
                    continue;
                }
                let mut finding = super::structured_output_tests::finding(
                    &path,
                    line,
                    &format!(
                        "{}{HEADLINE_SEP}{}",
                        f["title"].as_str().unwrap_or_default(),
                        f["detail"].as_str().unwrap_or_default()
                    ),
                    f["code"].as_str().unwrap_or_default(),
                );
                finding.rule_id = rule;
                for (k, v) in f["captures"].as_object().into_iter().flatten() {
                    if let Some(v) = v.as_str() {
                        finding.captures.insert(k.clone(), v.to_string());
                    }
                }
                out.push(finding);
            }
        }
    }
    out
}

/// The dotted names of `fix` (a member access or a `table.column`) the repository grounds only
/// as separate parts: no file spells the whole thing. Where the parts rule is more lenient than
/// the verbatim window was.
fn dotted_grounded_by_parts_only(
    fix: &str,
    repo: &RepoNames<'_>,
    files: &[(String, String)],
) -> Vec<String> {
    let backtick_re = regex::Regex::new(r"`([^`]+)`").expect("static regex");
    let mut out = Vec::new();
    for cap in backtick_re.captures_iter(fix) {
        let span = cap[1].trim();
        if span.contains(|c: char| c.is_whitespace() || c == '(' || c == '/') || !span.contains('.')
        {
            continue;
        }
        if is_file_like(span, false) {
            continue;
        }
        if repo.knows(span) && !files.iter().any(|(_, c)| c.contains(span)) {
            out.push(span.to_string());
        }
    }
    out
}

/// Replays every stored fix reply of the two efficiency runs against the REAL fixture repository
/// (the files the run scanned, read from disk), finding by finding: a fix passes "before" when
/// some finding's own 17-line evidence grounds every name it uses (the F1 checker), and "after"
/// when the evidence plus the repository does (the F3 checker). Prints both counts, the names
/// still unknown, whether the four real names the window rejected are known now, and the dotted
/// names the parts rule grounds although no file spells them whole; asserts the real inventions
/// are still flagged. Set `CAMERATA_STORED_RUNS` and `CAMERATA_REAL_REPO`, run with
/// `--ignored --nocapture`.
#[test]
#[ignore = "reads the stored run stores and the real fixture repository; set CAMERATA_STORED_RUNS and CAMERATA_REAL_REPO"]
fn the_stored_fix_replies_against_the_real_repository() {
    let (Ok(dir), Ok(root)) = (
        std::env::var("CAMERATA_STORED_RUNS"),
        std::env::var("CAMERATA_REAL_REPO"),
    ) else {
        return;
    };
    for state in ["state-sonnet-all-on", "state-haiku-check-all-on"] {
        let calls = read_calls(&dir, state);
        let files = real_repository(&root, &calls);
        let repo = RepoNames::new(&files);
        let findings = findings_of(&calls);
        let evidences: Vec<String> = findings
            .iter()
            .map(|f| fix_evidence_blob(f, &files))
            .collect();
        for name in REAL_NAMES_OUTSIDE_THE_WINDOW {
            println!("{state}: repository knows `{name}`: {}", repo.knows(name));
        }
        for name in INVENTIONS {
            println!("{state}: repository knows `{name}`: {}", repo.knows(name));
        }
        for name in [
            "NODE_EXTRA_CA_CERTS",
            "safeParse",
            "next/core-web-vitals",
            "aal",
            "createBrowserSupabaseClient",
            "README",
        ] {
            println!("{state}: repository knows `{name}`: {}", repo.knows(name));
        }
        let (mut fixes, mut before, mut after) = (0usize, 0usize, 0usize);
        let mut still_unknown = std::collections::BTreeMap::<String, usize>::new();
        let mut parts_only = std::collections::BTreeSet::new();
        let mut inventions_flagged = std::collections::BTreeSet::new();
        for call in calls.iter().filter(|c| c["kind"] == "fix") {
            for reply in reply_texts(call) {
                for fix in fix_texts(&reply) {
                    fixes += 1;
                    let passes_before = evidences
                        .iter()
                        .any(|e| find_ungrounded_identifier_in(&fix, e, None).is_none());
                    let passes_after = evidences
                        .iter()
                        .any(|e| find_ungrounded_identifier_in(&fix, e, Some(&repo)).is_none());
                    before += usize::from(passes_before);
                    after += usize::from(passes_after);
                    assert!(
                        passes_after || !passes_before,
                        "the repository never takes a grounded fix away: {fix}"
                    );
                    if !passes_after {
                        // The best evidence (fewest unknown names) says which names are left.
                        let unknown = evidences
                            .iter()
                            .map(|e| {
                                let mut names = Vec::new();
                                let mut rest = fix.clone();
                                while let Some(bad) =
                                    find_ungrounded_identifier_in(&rest, e, Some(&repo))
                                {
                                    names.push(bad.clone());
                                    rest = rest.replace(&bad, " ");
                                }
                                names
                            })
                            .min_by_key(Vec::len)
                            .unwrap_or_default();
                        for name in &unknown {
                            *still_unknown.entry(name.clone()).or_default() += 1;
                        }
                        for invented in INVENTIONS {
                            if fix.contains(invented)
                                && unknown
                                    .iter()
                                    .any(|n| n == invented || invented.contains(n.as_str()))
                            {
                                inventions_flagged.insert(invented);
                            }
                        }
                    } else {
                        for invented in INVENTIONS {
                            assert!(
                                !fix.contains(invented),
                                "{invented} is an invention and must not pass: {fix}"
                            );
                        }
                    }
                    parts_only.extend(dotted_grounded_by_parts_only(&fix, &repo, &files));
                }
            }
        }
        println!(
            "{state}: {} findings, {} files; {fixes} stored fixes: {before} grounded by a \
             finding's window alone (before), {after} by the window plus the repository (after)",
            findings.len(),
            files.len()
        );
        println!("{state}: names still unknown (name: fixes): {still_unknown:?}");
        println!("{state}: dotted names grounded by their parts only: {parts_only:?}");
        println!("{state}: inventions found and still flagged: {inventions_flagged:?}");
    }
}
