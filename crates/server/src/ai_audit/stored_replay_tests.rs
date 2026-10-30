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
