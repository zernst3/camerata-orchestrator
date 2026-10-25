//! Which files are assessed as the DEPLOYED rules: `firebase.json` resolution.
//!
//! This is the guard against the "multiple rules files" and "non-default path" traps. A
//! `firebase.json` may point at any file (`"rules": "rules/firestore.prod.rules"`, one per named
//! database or bucket), a monorepo keeps one per app, and a repository can hold dev, draft and
//! test rules files that are not what `firebase deploy` ships. A deterministic finding against a
//! file that is not the deployed one would be a false positive, so findings come only from the
//! files named here.

use super::json_lite::{self, Json};
use super::rules_parse::{parse_rules, Service};
use crate::extract::paths::is_test_path;

/// What a rules file is expected to hold, from where it was named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesKind {
    Firestore,
    Storage,
    Rtdb,
}

/// Why an assessed rules file may not be the one that deploys. Findings from such a file are
/// review-graded (held, not asserted) and carry the reason (owner call, 2026-10-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Doubt {
    /// It sits under an `examples`, `samples` or `docs` folder.
    ExamplePath,
    /// No `firebase.json` is in the repository to say what deploys.
    NoFirebaseJson,
    /// A `firebase.json` exists but does not reference it: it was found by its conventional name.
    NotReferenced,
}

impl Doubt {
    /// The sentence a finding about such a file ends with.
    pub fn sentence(self) -> &'static str {
        match self {
            Doubt::ExamplePath => "This file is under an examples, samples or docs folder, so it may be illustrative rather than the ruleset that deploys.",
            Doubt::NoFirebaseJson => "No `firebase.json` in this repository names this file, so it may not be the ruleset that deploys.",
            Doubt::NotReferenced => "The `firebase.json` beside it does not reference this file, so it may not be the ruleset that deploys.",
        }
    }
}

/// Folders whose rules files are examples, not deployments.
const EXAMPLE_FOLDERS: &[&str] = &["examples", "example", "samples", "sample", "docs"];

/// Whether `path` sits under a folder named for examples (a directory segment, whole word).
fn under_example_folder(path: &str) -> bool {
    let mut segments: Vec<&str> = path.split('/').collect();
    segments.pop();
    segments
        .iter()
        .any(|s| EXAMPLE_FOLDERS.contains(&s.to_ascii_lowercase().as_str()))
}

/// One rules file that is assessed as deployed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssessedFile {
    pub path: String,
    pub kind: RulesKind,
    /// The `database` / `bucket` / `target` / `instance` the file belongs to, when named.
    pub label: Option<String>,
    /// The `firebase.json` that named it (`None` when resolved by convention with none present).
    pub firebase_json: Option<String>,
    /// The `firebase.json` beside it could not be read, so the deployed file is unconfirmed.
    pub config_unreadable: bool,
    /// Why this file may not be the deployed one; empty when `firebase.json` names it.
    pub doubts: Vec<Doubt>,
}

/// A rules file that carries a Firebase `service` header but is not the deployed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreferencedFile {
    pub path: String,
    pub service: Service,
}

/// A rules file `firebase.json` names that is not among the readable files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingNamed {
    pub firebase_json: String,
    pub path: String,
}

/// The resolved Firebase file set of a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSet {
    pub assessed: Vec<AssessedFile>,
    pub unreferenced: Vec<UnreferencedFile>,
    /// `firebase.json` files that could not be read.
    pub unreadable_configs: Vec<String>,
    pub missing_named: Vec<MissingNamed>,
    /// At least one `firebase.json` is in the repository.
    pub has_firebase_json: bool,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `rel` resolved against `dir`, with `.` and `..` folded. A leading `/` is the repo root.
fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if rel.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// A rules file for tests or the emulator: never the deployed one unless `firebase.json` names it.
pub fn is_test_rules_path(path: &str) -> bool {
    is_test_path(path)
        || path
            .split('/')
            .any(|seg| seg == "emulator" || seg == "emulators")
}

/// What `firebase.json` names for one product: `(rules path, label)` pairs.
fn named(config: &Json, product: &str, label_keys: &[&str]) -> Vec<(String, Option<String>)> {
    let Some(member) = config.get(product) else {
        return Vec::new();
    };
    let entries: Vec<&Json> = match &member.value {
        Json::Array(items) => items.iter().collect(),
        other @ Json::Object(_) => vec![other],
        _ => Vec::new(),
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let rules = entry.get("rules")?.value.as_str()?.to_string();
            let label = label_keys.iter().find_map(|k| {
                entry
                    .get(k)
                    .and_then(|m| m.value.as_str())
                    .map(str::to_string)
            });
            Some((rules, label))
        })
        .collect()
}

/// Resolve the Firebase file set from `files` (every readable `(path, content)` pair).
pub fn resolve(files: &[(String, String)]) -> FileSet {
    let contents: std::collections::HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let mut set = FileSet::default();
    let mut config_dirs: Vec<String> = Vec::new();

    let mut configs: Vec<&str> = files
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| basename(p) == "firebase.json")
        .collect();
    configs.sort_unstable();
    set.has_firebase_json = !configs.is_empty();

    for config_path in configs {
        let dir = dir_of(config_path).to_string();
        config_dirs.push(dir.clone());
        let (value, errors) = json_lite::parse(contents[config_path]);
        let readable =
            errors.is_empty() && value.as_ref().is_some_and(|v| matches!(v, Json::Object(_)));
        if !readable {
            set.unreadable_configs.push(config_path.to_string());
        }
        let config = value.filter(|_| readable);
        for (product, kind, conventional, label_keys) in [
            (
                "firestore",
                RulesKind::Firestore,
                "firestore.rules",
                &["database"][..],
            ),
            (
                "storage",
                RulesKind::Storage,
                "storage.rules",
                &["bucket", "target"][..],
            ),
            (
                "database",
                RulesKind::Rtdb,
                "database.rules.json",
                &["instance", "target"][..],
            ),
        ] {
            let named_rules = config
                .as_ref()
                .map(|c| named(c, product, label_keys))
                .unwrap_or_default();
            if named_rules.is_empty() {
                // `firebase init` creates exactly these beside firebase.json.
                let path = join(&dir, conventional);
                if contents.contains_key(path.as_str()) && !is_test_rules_path(&path) {
                    push_unique(
                        &mut set.assessed,
                        AssessedFile {
                            path,
                            kind,
                            label: None,
                            firebase_json: Some(config_path.to_string()),
                            config_unreadable: !readable,
                            doubts: vec![Doubt::NotReferenced],
                        },
                    );
                }
                continue;
            }
            for (rules, label) in named_rules {
                let path = join(&dir, &rules);
                if contents.contains_key(path.as_str()) {
                    // Named by firebase.json: what deploys, whatever its folder.
                    push_unique(
                        &mut set.assessed,
                        AssessedFile {
                            path,
                            kind,
                            label,
                            firebase_json: Some(config_path.to_string()),
                            config_unreadable: false,
                            doubts: Vec::new(),
                        },
                    );
                } else {
                    set.missing_named.push(MissingNamed {
                        firebase_json: config_path.to_string(),
                        path,
                    });
                }
            }
        }
    }

    if !set.has_firebase_json {
        // No firebase.json at all: the conventional basenames anywhere, outside test paths.
        let mut paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        paths.sort_unstable();
        for path in paths {
            if is_test_rules_path(path) {
                continue;
            }
            let kind = match basename(path) {
                "firestore.rules" => RulesKind::Firestore,
                "storage.rules" => RulesKind::Storage,
                "database.rules.json" => RulesKind::Rtdb,
                _ => continue,
            };
            push_unique(
                &mut set.assessed,
                AssessedFile {
                    path: path.to_string(),
                    kind,
                    label: None,
                    firebase_json: None,
                    config_unreadable: false,
                    doubts: vec![Doubt::NoFirebaseJson],
                },
            );
        }
    }

    // Every other `.rules` file with a Firebase service header is unreferenced.
    let mut rules_paths: Vec<&str> = files
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| p.ends_with(".rules") && !is_test_rules_path(p))
        .filter(|p| !set.assessed.iter().any(|a| a.path == *p))
        .collect();
    rules_paths.sort_unstable();
    for path in rules_paths {
        if let Some(service @ (Service::Firestore | Service::Storage)) =
            parse_rules(contents[path]).service
        {
            set.unreferenced.push(UnreferencedFile {
                path: path.to_string(),
                service,
            });
        }
    }
    for file in &mut set.assessed {
        if under_example_folder(&file.path) {
            file.doubts.insert(0, Doubt::ExamplePath);
        }
    }
    set.assessed.sort_by(|a, b| a.path.cmp(&b.path));
    set
}

fn push_unique(list: &mut Vec<AssessedFile>, file: AssessedFile) {
    if !list.iter().any(|f| f.path == file.path) {
        list.push(file);
    }
}
