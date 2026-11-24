//! The key-path decision table and the walker that re-types every JSON leaf through it.
//!
//! A key path is the input's name (`run`, `ledger`, `failures`) followed by `.field` for a
//! struct field, `[]` for any array element, `[0]` for one tuple position, and `.*` for any key
//! of a data map (a map whose keys are data, such as a reason or a rule id; its keys are
//! re-typed by the map's [`KeyClass`]). Every leaf whose path has no entry is dropped and its
//! path is listed under `undecided`: the table fails closed, so a field the saved types gain
//! later stays out of every bundle until someone decides it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::{Map, Value};

mod table;

use super::sanitize::{
    engine_location, hash_path, repo_label, repo_ordinal, text_skeleton, PathTokens,
};

/// The serde names of every enum a bundle keeps. `every_enum_list_names_real_serde_variants`
/// pins each list to its type.
pub mod enums {
    pub const RUN_STATUS: &[&str] = &["running", "complete", "incomplete"];
    pub const RUN_ORIGIN: &[&str] = &["cli", "cockpit"];
    pub const PART_KIND: &[&str] = &[
        "tools",
        "scan",
        "calibration",
        "alternatives",
        "detail",
        "fix",
        "group_review",
        "other",
        "merge",
        "export",
    ];
    pub const PART_STATUS: &[&str] = &["pending", "running", "done", "failed"];
    pub const RULE_TIER: &[&str] = &[
        "deterministic",
        "architectural",
        "external_tool",
        "semantic",
        "unloaded",
    ];
    pub const SKIP_REASON: &[&str] = &[
        "oversize",
        "beyond_file_cap",
        "not_text",
        "excluded_by_agreement",
        "long_line",
        "excluded_never_read",
        "generated",
        "generated_unsearched",
        "not_inspected_type",
        "not_inspected_versions_only",
    ];
    pub const STAGE: &[&str] = &[
        "scan_pass",
        "alternatives",
        "detail_regeneration",
        "calibration",
        "group_review",
        "fix_generation",
        "deep_tier",
        "clone_judge",
        "finder",
        "duplicate_check",
    ];
    pub const FAILURE_CLASS: &[&str] = &[
        "transport",
        "auth_or_billing",
        "content",
        "spend_limit",
        "outage_stop",
        "io",
        "rate_limited",
    ];
    pub const VERDICT: &[&str] = &["same", "different", "unclear"];
    /// `ProjectBackend`, as `ModelsRecord.backend` and an attempt's backend hold it.
    pub const BACKEND: &[&str] = &["cli", "api"];
    /// `ScanMode`, lower-cased by its `Debug` name (it has no serde form).
    pub const SCAN_MODE: &[&str] = &["sequential", "parallel", "batch"];
    pub const FINDER: &[&str] = &["agent", "engine"];
    pub const LINE_ENDING: &[&str] = &["lf", "crlf", "mixed", "none"];
    /// The ledger stage names the engine writes (`record_stage` and its callers, and the
    /// constants `UNFOLD_STAGE`, `FIX_MERGE_STAGE`, `PLACE_MERGE_STAGE`, `LEDGER_STAGE`,
    /// `CONSENSUS_STAGE` and `STAGE_SAMPLE_NAME`). A name not listed is rejected, never kept
    /// as text. `the_ledger_stage_list_holds_the_engines_stage_constants` pins the constants.
    pub const LEDGER_STAGE: &[&str] = &[
        "cross-family-merge",
        "ai-location-merge",
        "external-tool-preview-merge",
        "evidence-gate-synthesis",
        "report-build-partition",
        "runtime-audit-merge",
        "unfold-also-locations",
        "dependency-audit-merge",
        "ai-group-review",
        "ai-calibration",
        "fix-merge",
        "place-merge",
        "consensus",
    ];

    /// Every list, for the leak check's set of engine words.
    pub const ALL: &[&[&str]] = &[
        RUN_STATUS,
        RUN_ORIGIN,
        PART_KIND,
        PART_STATUS,
        RULE_TIER,
        SKIP_REASON,
        STAGE,
        FAILURE_CLASS,
        VERDICT,
        BACKEND,
        SCAN_MODE,
        FINDER,
        LINE_ENDING,
        LEDGER_STAGE,
    ];
}

/// What a leaf becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueClass {
    /// A JSON number, as it is.
    Number,
    /// A JSON bool, as it is.
    Bool,
    /// A number or a bool, as it is; a string is rejected.
    Scalar,
    /// RFC 3339, parsed and printed again in UTC; an empty string stays empty.
    Timestamp,
    /// The value only when it is one of these serde names.
    Enum(&'static [&'static str]),
    /// Lower-case hex of at most 64 characters (a commit or a digest); anything else is
    /// rejected.
    Hex,
    /// A version or a corpus digest: `^[A-Za-z0-9.+_-]{0,64}$`; anything else is rejected.
    Version,
    /// The id when the shipped corpus has it, else `rule:<12 hex>` (salted).
    RuleId,
    /// The value when it matches `^[a-z0-9.:\[\]-]{1,64}$`.
    ModelId,
    /// `p:<12 hex>.<ext>@<depth>`, or the repo-relative path under `--named-paths`.
    Path,
    /// `repo-N`, by order in `RunRecord.repos`.
    Repo,
    /// The word skeleton (see [`super::sanitize::text_skeleton`]).
    Text,
    /// `file:line` in Camerata's own code (see [`super::sanitize::engine_location`]).
    EngineLocation,
    /// An engine-written value (a version, a fingerprint, a stage name), as it is, with any
    /// secret span replaced by `[secret]`.
    Allowed,
    /// A provider error type, when it matches `^[a-z_]{1,40}$`.
    ErrorType,
    /// A plan part's id (`<repo>:<kind>`): the repo's label plus the kind.
    PartId,
    /// A finder piece's name: `piece-N`, by first appearance in the bundle.
    Piece,
    /// Nothing: the leaf is left out.
    Drop,
    /// The value exactly as it is. Only a deliberately wrong table in a test uses it, to prove
    /// the leak check refuses what such a table lets through.
    #[cfg(test)]
    Verbatim,
}

/// What the keys of a data map become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    /// The word skeleton.
    Text,
    /// As [`ValueClass::RuleId`].
    RuleId,
    /// As [`ValueClass::ModelId`]; a key that does not match drops its entry.
    ModelId,
    /// An engine-written identifier (`invented_precondition`): kept when it is a lower-case
    /// snake-case word, else the word skeleton.
    EngineKey,
    /// The key exactly as it is. Only a deliberately wrong table in a test uses it.
    #[cfg(test)]
    Verbatim,
}

/// One entry of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Leaf(ValueClass),
    /// A data map; its entries' values are decided under `<path>.*`.
    Keys(KeyClass),
    /// `RunRecord.args`: key names kept, bools and numbers kept, every string replaced by its
    /// length in characters.
    Args,
}

/// The key-path decision table.
#[derive(Debug, Clone)]
pub struct DecisionTable {
    nodes: HashMap<String, Node>,
    /// Every proper prefix of an entry, so the walker knows a struct it may descend into.
    prefixes: HashSet<String>,
}

/// The segments of a key path: `.field`, `.*`, `[]` or `[n]`.
fn segments(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in path.char_indices() {
        if (c == '.' || c == '[') && i > start {
            out.push(&path[start..i]);
            start = i;
        }
    }
    out.push(&path[start..]);
    out
}

impl DecisionTable {
    fn empty() -> Self {
        Self {
            nodes: HashMap::new(),
            prefixes: HashSet::new(),
        }
    }

    /// This table with `path` decided as `node` (replacing any decision it had).
    pub fn with(mut self, path: &str, node: Node) -> Self {
        self.insert(path, node);
        self
    }

    fn insert(&mut self, path: &str, node: Node) {
        let segs = segments(path);
        let mut prefix = String::new();
        for seg in &segs[..segs.len() - 1] {
            prefix.push_str(seg);
            self.prefixes.insert(prefix.clone());
        }
        self.nodes.insert(path.to_string(), node);
    }

    fn leaves(&mut self, base: &str, fields: &[(&str, ValueClass)]) {
        for (field, class) in fields {
            self.insert(&format!("{base}.{field}"), Node::Leaf(*class));
        }
    }

    /// The decision for a key path, if any.
    pub fn get(&self, path: &str) -> Option<Node> {
        self.nodes.get(path).copied()
    }

    /// Whether the walker may visit `path`: it is decided, or a decided path lies under it.
    pub fn knows(&self, path: &str) -> bool {
        self.nodes.contains_key(path) || self.prefixes.contains(path)
    }

    /// Every field name the table spells out, for the leak check's set of engine words.
    pub fn field_names(&self) -> BTreeSet<String> {
        self.nodes
            .keys()
            .chain(self.prefixes.iter())
            .flat_map(|p| {
                segments(p)
                    .into_iter()
                    .filter_map(|s| s.strip_prefix('.'))
                    .filter(|s| *s != "*")
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The shipped table (design section 2).
    pub fn standard() -> Self {
        static TABLE: OnceLock<DecisionTable> = OnceLock::new();
        TABLE.get_or_init(table::build_standard).clone()
    }
}

// ── The walker ───────────────────────────────────────────────────────────────────────────────

/// What the walker did with the input's leaves. `kept + dropped + rejected + undecided` is
/// every leaf of the inputs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Coverage {
    pub leaves: u64,
    /// Re-typed into the bundle.
    pub kept: u64,
    /// Left out by a `Drop` decision.
    pub dropped: u64,
    /// Decided, but the value did not fit its class (an unknown enum value, a bad time).
    pub rejected: u64,
    /// No decision: left out and listed.
    pub undecided: u64,
}

/// One bundle's walk over its inputs.
pub struct Walker<'w, 's> {
    table: &'w DecisionTable,
    paths: &'w mut PathTokens<'s>,
    known_rule_ids: &'w BTreeSet<String>,
    pieces: BTreeMap<String, usize>,
    pub undecided: BTreeSet<String>,
    /// The key paths of rejected values (never the values).
    pub rejected: BTreeSet<String>,
    pub coverage: Coverage,
}

fn leaf_count(v: &Value) -> u64 {
    match v {
        Value::Object(m) => m.values().map(leaf_count).sum(),
        Value::Array(a) => a.iter().map(leaf_count).sum(),
        _ => 1,
    }
}

/// A field name as the undecided list may show it: a snake-case identifier, or `<key>`.
fn listed_field(k: &str) -> &str {
    let ok = !k.is_empty()
        && k.len() <= 64
        && k.chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && k.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        k
    } else {
        "<key>"
    }
}

pub(crate) fn model_id_ok(s: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[a-z0-9.:\[\]-]{1,64}$").expect("model id pattern"))
        .is_match(s)
}

fn error_type_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

fn engine_key_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `text` with every secret span replaced by `[secret]`.
pub(crate) fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for m in camerata_secrets::detect_secrets(text) {
        if m.start < at || m.end > text.len() {
            continue;
        }
        out.push_str(&text[at..m.start]);
        out.push_str("[secret]");
        at = m.end;
    }
    out.push_str(&text[at..]);
    out
}

impl<'w, 's> Walker<'w, 's> {
    pub fn new(
        table: &'w DecisionTable,
        paths: &'w mut PathTokens<'s>,
        known_rule_ids: &'w BTreeSet<String>,
    ) -> Self {
        Self {
            table,
            paths,
            known_rule_ids,
            pieces: BTreeMap::new(),
            undecided: BTreeSet::new(),
            rejected: BTreeSet::new(),
            coverage: Coverage::default(),
        }
    }

    /// The path tokens this walk made, for the lookup file and the leak check.
    pub fn paths(&mut self) -> &mut PathTokens<'s> {
        self.paths
    }

    /// Walk one input rooted at `root` (`run`, `ledger` or `failures`).
    pub fn walk(&mut self, root: &str, value: &Value) -> Value {
        self.coverage.leaves += leaf_count(value);
        if !self.table.knows(root) {
            self.undecided.insert(root.to_string());
            self.coverage.undecided += leaf_count(value);
            return Value::Null;
        }
        self.node(root, value, 0).unwrap_or(Value::Null)
    }

    /// The repo label of a repo name.
    pub fn repo(&self, name: &str) -> String {
        if name.is_empty() {
            return String::new();
        }
        match repo_ordinal(self.paths.repos(), name) {
            Some(n) => repo_label(n),
            None => {
                let h = hash_path(self.paths.salt(), 0, &format!("repo\u{0}{name}"));
                format!("repo-x{}", &h[..6])
            }
        }
    }

    fn rule_id(&self, id: &str) -> String {
        if id.is_empty() || self.known_rule_ids.contains(id) {
            return id.to_string();
        }
        format!(
            "rule:{}",
            hash_path(self.paths.salt(), 0, &format!("rule\u{0}{id}"))
        )
    }

    fn node(&mut self, pat: &str, v: &Value, ordinal: u32) -> Option<Value> {
        if self.table.get(pat) == Some(Node::Args) {
            self.coverage.kept += leaf_count(v);
            return Some(args(v));
        }
        match v {
            Value::Object(map) => Some(self.object(pat, map, ordinal)),
            Value::Array(items) => Some(self.array(pat, items, ordinal)),
            leaf => match self.table.get(pat) {
                Some(Node::Leaf(class)) => self.leaf(pat, class, leaf, ordinal),
                // An absent struct or map (`None`) carries nothing.
                _ if leaf.is_null() && self.table.knows(pat) => {
                    self.coverage.kept += 1;
                    Some(Value::Null)
                }
                Some(_) => {
                    self.rejected.insert(pat.to_string());
                    self.coverage.rejected += 1;
                    None
                }
                None => {
                    self.undecided.insert(pat.to_string());
                    self.coverage.undecided += 1;
                    None
                }
            },
        }
    }

    fn object(&mut self, pat: &str, map: &Map<String, Value>, ordinal: u32) -> Value {
        // A record naming its repo keys its paths by that repo.
        let ordinal = map
            .get("repo")
            .and_then(Value::as_str)
            .map_or(ordinal, |r| {
                repo_ordinal(self.paths.repos(), r).unwrap_or(0)
            });
        let mut out = Map::new();
        if let Some(Node::Keys(class)) = self.table.get(pat) {
            let child = format!("{pat}.*");
            for (k, v) in map {
                let Some(key) = self.key(class, k, ordinal) else {
                    self.rejected.insert(child.clone());
                    self.coverage.rejected += leaf_count(v);
                    continue;
                };
                if let Some(value) = self.node(&child, v, ordinal) {
                    let mut name = key.clone();
                    let mut n = 2;
                    while out.contains_key(&name) {
                        name = format!("{key} #{n}");
                        n += 1;
                    }
                    out.insert(name, value);
                }
            }
            return Value::Object(out);
        }
        for (k, v) in map {
            let child = format!("{pat}.{k}");
            if !self.table.knows(&child) {
                self.undecided.insert(format!("{pat}.{}", listed_field(k)));
                self.coverage.undecided += leaf_count(v);
                continue;
            }
            if let Some(value) = self.node(&child, v, ordinal) {
                out.insert(k.clone(), value);
            }
        }
        Value::Object(out)
    }

    fn array(&mut self, pat: &str, items: &[Value], ordinal: u32) -> Value {
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let at = format!("{pat}[{i}]");
            let child = if self.table.knows(&at) {
                at
            } else {
                format!("{pat}[]")
            };
            if !self.table.knows(&child) {
                self.undecided.insert(child);
                self.coverage.undecided += leaf_count(item);
                out.push(Value::Null);
                continue;
            }
            // A dropped or rejected element stays as `null`, so positions keep their meaning.
            out.push(self.node(&child, item, ordinal).unwrap_or(Value::Null));
        }
        Value::Array(out)
    }

    fn key(&mut self, class: KeyClass, k: &str, ordinal: u32) -> Option<String> {
        Some(match class {
            KeyClass::Text => text_skeleton(k, self.paths, ordinal),
            KeyClass::RuleId => self.rule_id(k),
            KeyClass::ModelId => {
                if !model_id_ok(k) {
                    return None;
                }
                k.to_string()
            }
            KeyClass::EngineKey => {
                if engine_key_ok(k) {
                    k.to_string()
                } else {
                    text_skeleton(k, self.paths, ordinal)
                }
            }
            #[cfg(test)]
            KeyClass::Verbatim => k.to_string(),
        })
    }

    fn leaf(&mut self, pat: &str, class: ValueClass, v: &Value, ordinal: u32) -> Option<Value> {
        if class == ValueClass::Drop {
            self.coverage.dropped += 1;
            return None;
        }
        if v.is_null() {
            self.coverage.kept += 1;
            return Some(Value::Null);
        }
        let got = self.retype(class, v, ordinal);
        match got {
            Some(_) => self.coverage.kept += 1,
            None => {
                self.rejected.insert(pat.to_string());
                self.coverage.rejected += 1;
            }
        }
        got
    }

    fn retype(&mut self, class: ValueClass, v: &Value, ordinal: u32) -> Option<Value> {
        use ValueClass::*;
        let s = v.as_str();
        Some(match class {
            Number => v.is_number().then(|| v.clone())?,
            Bool => v.is_boolean().then(|| v.clone())?,
            Scalar => (v.is_number() || v.is_boolean()).then(|| v.clone())?,
            Timestamp => {
                let s = s?;
                if s.is_empty() {
                    return Some(Value::String(String::new()));
                }
                let t = chrono::DateTime::parse_from_rfc3339(s).ok()?;
                Value::String(
                    t.with_timezone(&chrono::Utc)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                )
            }
            Enum(list) => {
                let s = s?;
                list.contains(&s).then(|| v.clone())?
            }
            RuleId => Value::String(self.rule_id(s?)),
            ModelId => {
                let s = s?;
                if s.is_empty() {
                    return Some(Value::String(String::new()));
                }
                model_id_ok(s).then(|| v.clone())?
            }
            Path => {
                let s = s?;
                if s.is_empty() {
                    return Some(Value::String(String::new()));
                }
                Value::String(self.paths.leaf(s, ordinal))
            }
            Repo => Value::String(self.repo(s?)),
            Text => Value::String(text_skeleton(s?, self.paths, ordinal)),
            EngineLocation => Value::String(engine_location(s?)?),
            Allowed => match s {
                Some(s) => Value::String(redact_secrets(s)),
                None => (v.is_number() || v.is_boolean()).then(|| v.clone())?,
            },
            Hex => {
                let s = s?;
                (s.len() <= 64 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
                    .then(|| v.clone())?
            }
            Version => {
                let s = s?;
                (s.len() <= 64
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-')))
                .then(|| v.clone())?
            }
            ErrorType => {
                let s = s?;
                error_type_ok(s).then(|| v.clone())?
            }
            PartId => {
                let s = s?;
                let (repo, kind) = match s.rsplit_once(':') {
                    Some((r, k)) => (Some(r), k),
                    None => (None, s),
                };
                if !enums::PART_KIND.contains(&kind) {
                    return None;
                }
                match repo {
                    Some(r) => Value::String(format!("{}:{kind}", self.repo(r))),
                    None => Value::String(kind.to_string()),
                }
            }
            Piece => {
                let s = s?;
                let next = self.pieces.len() + 1;
                let n = *self.pieces.entry(s.to_string()).or_insert(next);
                Value::String(format!("piece-{n}"))
            }
            Drop => return None,
            #[cfg(test)]
            Verbatim => v.clone(),
        })
    }
}

/// `RunRecord.args`: key names, bools and numbers kept; every string replaced by its length.
fn args(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let mut name = listed_field(k).to_string();
                let mut n = 2;
                while out.contains_key(&name) {
                    name = format!("{} #{n}", listed_field(k));
                    n += 1;
                }
                out.insert(name, args(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(args).collect()),
        Value::String(s) => Value::from(s.chars().count()),
        other => other.clone(),
    }
}
