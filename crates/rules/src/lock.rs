//! Rule content hashing and `CORPUS_LOCK.toml`: making `Rule::version` mechanical.
//!
//! Design: `docs/design/2026-10-20_corpus-staleness-check.md`, "Amendment 2026-10-21", part 2.
//!
//! A `version` field nobody is forced to bump rots. So:
//!
//! - [`Rule::content_hash`] fingerprints the WHOLE rule (every field, options included), minus
//!   `version` and `last_reviewed`, as `sha256:<hex>`. Excluding those two means bumping the
//!   version, or recording a review, does not change the hash.
//! - `crates/rules/CORPUS_LOCK.toml` records `(version, hash)` for every rule.
//! - [`check_lock`] (the gate, run by a rules-crate test) fails a rule whose hash differs from
//!   the lock while its version is unchanged, and fails every other disagreement between the
//!   corpus and the lock. The lock never moves on its own: `camerata corpus lock`
//!   ([`CorpusLock::regenerate`]) is the only writer, and it refuses a rule whose hash changed
//!   without a version bump, so the CLI cannot launder a missed bump.
//!
//! # The canonical serialization
//!
//! The hash preimage is `"camerata-rule-hash/v1\n"` followed by a canonical JSON rendering of
//! the rule, built by [`Rule::canonical_form`]. It is stable across builds, platforms and
//! dependency upgrades because nothing in it is delegated to a library that does not promise
//! stability:
//!
//! - Keys are emitted in sorted order at every level, by this module's own renderer (never by a
//!   map type whose iteration order depends on a cargo feature).
//! - There is no insignificant whitespace.
//! - String escaping is implemented here and matches `json.dumps(..., ensure_ascii=False)`:
//!   `"` `\` and the C0 controls are escaped, everything else is emitted verbatim as UTF-8.
//! - A field that is `None`, empty, or `false` is OMITTED. An absent optional field and an empty
//!   one are the same rule, and a field added later that no existing rule sets leaves every
//!   existing hash untouched.
//! - Lists keep their authored order (option order is meaningful to the UI).
//!
//! The `/v1` tag is the format version. Changing the canonical form in ANY way changes every
//! rule's hash, which is a deliberate migration (bump the tag, regenerate the lock), never an
//! incidental side effect; `the_canonical_form_and_hash_match_the_independent_oracle` pins the
//! bytes against a Python oracle so it cannot happen quietly.
//!
//! [`Rule::canonical_form`] destructures `Rule` (and every nested type) WITHOUT a `..` rest
//! pattern. Adding a field to any of them is therefore a compile error here until someone
//! decides whether it is part of the rule's content, which is the point: a hash that silently
//! ignored a new field would be the original drift-detector defect again.
//!
//! `layer` and `[[emits]]` are parsed but not carried onto [`Rule`] (they are accepted and
//! consumed by nothing), so an edit to only those is not a content change and does not move the
//! hash. Comments and formatting in the TOML never reach [`Rule`] either.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    AppliesTo, Candidates, EscalationSeverity, EscalationSpec, Rule, RuleOption, RuleSet,
    RuleSource, StackException, VerifiedProvenance,
};

/// Prefix of every rule content hash: `sha256:<64 lowercase hex>`. A stored hash WITHOUT this
/// prefix predates [`Rule::content_hash`] (it came from the old `DefaultHasher` fingerprint)
/// and is a legacy baseline, never comparable to a current hash.
pub const RULE_HASH_PREFIX: &str = "sha256:";

/// Domain-separation tag and canonical-format version, hashed ahead of the canonical form.
const PREIMAGE_TAG: &str = "camerata-rule-hash/v1\n";

/// The bundled lock path, `crates/rules/CORPUS_LOCK.toml`, resolved from the crate's manifest
/// dir (the sibling of `principles/`). Mirrors [`crate::corpus_freshness_path`].
pub fn corpus_lock_path() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/CORPUS_LOCK.toml"))
}

// ────────────────────────────────────────────────────────────────────────────
// Canonical serialization
// ────────────────────────────────────────────────────────────────────────────

/// A canonical value tree. Private: the only public surface is the rendered string.
enum Canon {
    Str(String),
    Bool(bool),
    Arr(Vec<Canon>),
    Obj(Vec<(&'static str, Canon)>),
}

impl Canon {
    /// Render with sorted keys and no whitespace.
    fn render(&self, out: &mut String) {
        match self {
            Canon::Str(s) => render_string(s, out),
            Canon::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Canon::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.render(out);
                }
                out.push(']');
            }
            Canon::Obj(fields) => {
                let mut sorted: Vec<&(&'static str, Canon)> = fields.iter().collect();
                sorted.sort_by_key(|(k, _)| *k);
                out.push('{');
                for (i, (key, value)) in sorted.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    render_string(key, out);
                    out.push(':');
                    value.render(out);
                }
                out.push('}');
            }
        }
    }
}

/// JSON string escaping, byte-for-byte what `json.dumps(s, ensure_ascii=False)` produces.
fn render_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Builder for an object that applies the omit-when-empty policy in one place.
#[derive(Default)]
struct ObjBuilder(Vec<(&'static str, Canon)>);

impl ObjBuilder {
    /// A field that is always present (identity fields).
    fn always(&mut self, key: &'static str, value: &str) {
        self.0.push((key, Canon::Str(value.to_string())));
    }

    /// A text field, omitted when empty.
    fn text(&mut self, key: &'static str, value: &str) {
        if !value.is_empty() {
            self.always(key, value);
        }
    }

    /// An optional text field, omitted when `None` or empty.
    fn opt_text(&mut self, key: &'static str, value: &Option<String>) {
        if let Some(v) = value {
            self.text(key, v);
        }
    }

    /// A flag, omitted when `false`.
    fn flag(&mut self, key: &'static str, value: bool) {
        if value {
            self.0.push((key, Canon::Bool(true)));
        }
    }

    /// An optional flag, omitted when `None`. `Some(false)` is a real declaration (the rule
    /// said `inspectable = false`), so unlike [`Self::flag`] it is kept.
    fn opt_flag(&mut self, key: &'static str, value: Option<bool>) {
        if let Some(v) = value {
            self.0.push((key, Canon::Bool(v)));
        }
    }

    /// A list of strings, omitted when empty.
    fn strings(&mut self, key: &'static str, values: &[String]) {
        if !values.is_empty() {
            self.0.push((
                key,
                Canon::Arr(values.iter().map(|v| Canon::Str(v.clone())).collect()),
            ));
        }
    }

    /// A list of objects, omitted when empty.
    fn objects(&mut self, key: &'static str, values: Vec<Canon>) {
        if !values.is_empty() {
            self.0.push((key, Canon::Arr(values)));
        }
    }

    fn nested(&mut self, key: &'static str, value: Canon) {
        self.0.push((key, value));
    }

    fn build(self) -> Canon {
        Canon::Obj(self.0)
    }
}

fn canon_option(option: &RuleOption) -> Canon {
    // Exhaustive: no `..`. A new `RuleOption` field must be classified here.
    let RuleOption {
        id,
        label,
        directive,
        why,
        remediation,
        finding_headline,
        finding_detail,
        escalation,
        effort,
        violates,
    } = option;
    let mut o = ObjBuilder::default();
    o.always("id", id);
    o.always("label", label);
    o.text("directive", directive);
    o.text("why", why);
    o.opt_text("remediation", remediation);
    o.opt_text("finding_headline", finding_headline);
    o.opt_text("finding_detail", finding_detail);
    if let Some(spec) = escalation {
        o.nested("escalation", canon_escalation(spec));
    }
    o.opt_text("effort", effort);
    o.flag("violates", *violates);
    o.build()
}

fn canon_escalation(spec: &EscalationSpec) -> Canon {
    let EscalationSpec {
        condition,
        severity,
    } = spec;
    let mut o = ObjBuilder::default();
    o.always("condition", condition);
    // The same kebab-case spellings the TOML uses.
    o.always(
        "severity",
        match severity {
            EscalationSeverity::SoftFlag => "soft-flag",
            EscalationSeverity::HardPause => "hard-pause",
        },
    );
    o.build()
}

fn canon_source(source: &RuleSource) -> Canon {
    let RuleSource {
        url,
        title,
        linter,
        relevance,
    } = source;
    let mut o = ObjBuilder::default();
    o.always("url", url);
    o.always("title", title);
    o.opt_text("linter", linter);
    o.opt_text("relevance", relevance);
    o.build()
}

fn canon_verified(verified: &VerifiedProvenance) -> Canon {
    let VerifiedProvenance { by, at, against } = verified;
    let mut o = ObjBuilder::default();
    o.always("by", by);
    o.always("at", at);
    o.strings("against", against);
    o.build()
}

fn canon_applies_to(applies_to: &AppliesTo) -> Canon {
    let AppliesTo {
        paths,
        languages,
        contains_any,
    } = applies_to;
    // Sorted and de-duplicated: the three lists are unordered sets, so reordering an entry is
    // not a change to the rule and must not move its hash (a version bump would be spurious).
    let sorted = |values: &[String]| {
        let mut v = values.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };
    let mut o = ObjBuilder::default();
    o.strings("paths", &sorted(paths));
    o.strings("languages", &sorted(languages));
    o.strings("contains_any", &sorted(contains_any));
    o.build()
}

fn canon_candidates(candidates: &Candidates) -> Canon {
    let Candidates { locator } = candidates;
    let mut o = ObjBuilder::default();
    o.always("locator", locator.name());
    o.build()
}

fn canon_stack_exception(exception: &StackException) -> Canon {
    let StackException {
        framework,
        path_glob,
        note,
    } = exception;
    let mut o = ObjBuilder::default();
    o.always("framework", framework);
    o.always("path_glob", path_glob);
    o.text("note", note);
    o.build()
}

impl Rule {
    /// The canonical serialization of the WHOLE rule that [`Rule::content_hash`] hashes. See the
    /// module docs for the format and why it is stable. `version` and `last_reviewed` are
    /// deliberately not part of it.
    pub fn canonical_form(&self) -> String {
        // Exhaustive: no `..`. A new `Rule` field must be classified here as content (hashed)
        // or metadata (named and ignored, like the two below).
        let Rule {
            id,
            title,
            enforcement,
            domain,
            summary,
            decision_question,
            decision_why,
            options,
            default_option,
            verification,
            sources,
            verified,
            opt_in_only,
            layer3_only,
            stack_exceptions,
            extra_domains,
            kind,
            inspectable,
            max_severity,
            min_severity,
            deviation_ceiling,
            cwe,
            asvs,
            superseded_since,
            current_recommendation,
            applies_to,
            candidates,
            // Bookkeeping about the rule, not part of it: bumping the version or recording a
            // review must not change the hash the version is checked against.
            version: _,
            last_reviewed: _,
        } = self;

        let mut o = ObjBuilder::default();
        o.always("id", &id.0);
        o.always("title", title);
        o.always("enforcement", enforcement.as_str());
        o.always("domain", domain);
        o.text("summary", summary);
        o.opt_text("decision_question", decision_question);
        o.opt_text("decision_why", decision_why);
        o.objects("options", options.iter().map(canon_option).collect());
        o.opt_text("default_option", default_option);
        o.always("verification", verification.as_str());
        o.objects("sources", sources.iter().map(canon_source).collect());
        if let Some(v) = verified {
            o.nested("verified", canon_verified(v));
        }
        o.flag("opt_in_only", *opt_in_only);
        o.flag("layer3_only", *layer3_only);
        o.objects(
            "stack_exceptions",
            stack_exceptions.iter().map(canon_stack_exception).collect(),
        );
        o.strings("extra_domains", extra_domains);
        if let Some(k) = kind {
            o.always("kind", k.as_str());
        }
        o.opt_flag("inspectable", *inspectable);
        o.opt_text("max_severity", max_severity);
        o.opt_text("min_severity", min_severity);
        o.opt_text("deviation_ceiling", deviation_ceiling);
        o.strings("cwe", cwe);
        o.strings("asvs", asvs);
        o.opt_text("superseded_since", superseded_since);
        o.opt_text("current_recommendation", current_recommendation);
        if let Some(a) = applies_to {
            o.nested("applies_to", canon_applies_to(a));
        }
        if let Some(c) = candidates {
            o.nested("candidates", canon_candidates(c));
        }

        let mut out = String::new();
        o.build().render(&mut out);
        out
    }

    /// `sha256:<hex>` over the canonical form of the whole rule, EXCLUDING `version` and
    /// `last_reviewed`. Two builds of the same rule always produce the same string; any change
    /// to the rule's content (an option, a directive, a source, a flag) produces a different
    /// one; a version bump or a review date does not.
    pub fn content_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(PREIMAGE_TAG.as_bytes());
        hasher.update(self.canonical_form().as_bytes());
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(RULE_HASH_PREFIX.len() + digest.len() * 2);
        hex.push_str(RULE_HASH_PREFIX);
        for byte in digest {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }
}

// ────────────────────────────────────────────────────────────────────────────
// The corpus digest
// ────────────────────────────────────────────────────────────────────────────

/// Domain-separation tag and format version for [`corpus_version`], hashed ahead of the entries.
/// Distinct from [`PREIMAGE_TAG`] so a corpus digest can never collide with a rule hash.
const CORPUS_DIGEST_TAG: &str = "camerata-corpus-digest/v1\n";

/// Render a version for the digest preimage: the number, or `-` for a rule that declares none
/// (so an unversioned rule still contributes, and cannot be confused with version 0).
fn version_token(version: Option<u32>) -> String {
    version.map_or_else(|| "-".to_string(), |v| v.to_string())
}

/// The digest over `(id, version, hash)` entries. Entries are sorted before hashing, so the
/// result does not depend on the order they are supplied in. Each entry is one
/// `id TAB version TAB hash LF` line after [`CORPUS_DIGEST_TAG`]; none of the three fields can
/// contain a tab or newline (ids are `[A-Z0-9-]`, a hash is `sha256:<hex>`), so the framing is
/// unambiguous.
fn corpus_digest(mut entries: Vec<(String, Option<u32>, String)>) -> String {
    entries.sort();
    let mut hasher = Sha256::new();
    hasher.update(CORPUS_DIGEST_TAG.as_bytes());
    for (id, version, hash) in &entries {
        hasher.update(format!("{id}\t{}\t{hash}\n", version_token(*version)).as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(RULE_HASH_PREFIX.len() + digest.len() * 2);
    hex.push_str(RULE_HASH_PREFIX);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// A deterministic fingerprint of the rule corpus: `sha256:<hex>` over the sorted
/// `(id, version, content hash)` of every rule, which is exactly the content of
/// `CORPUS_LOCK.toml`. Two builds of the same corpus produce the same string on any machine, in
/// any load order; a changed rule (its [`Rule::content_hash`] moves) or a bumped `version`
/// produces a different one, and a recorded review date (`Rule::last_reviewed`) does not.
///
/// This is the client-visible "corpus version" of a product export: not a hand-maintained
/// version string that can rot, but the corpus's own content. A rule that declares no `version`
/// still contributes (as `-`), so the digest is total over any corpus the loader accepts; the
/// lock gate, not this function, is what rejects an unversioned rule.
pub fn corpus_version(set: &RuleSet) -> String {
    corpus_digest(
        set.iter()
            .map(|rule| (rule.id.0.clone(), rule.version, rule.content_hash()))
            .collect(),
    )
}

// ────────────────────────────────────────────────────────────────────────────
// The lock file
// ────────────────────────────────────────────────────────────────────────────

/// One rule's line in the lock: the version and content hash it had when the lock was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    pub version: u32,
    /// `sha256:<64 lowercase hex>`, see [`Rule::content_hash`].
    pub hash: String,
}

/// `crates/rules/CORPUS_LOCK.toml`, parsed. Keyed by rule id; a `BTreeMap` so iteration and
/// [`CorpusLock::render`] are sorted regardless of corpus load order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CorpusLock {
    pub rules: BTreeMap<String, LockEntry>,
}

#[derive(Debug, Error)]
pub enum LockError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("CORPUS_LOCK.toml does not parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("CORPUS_LOCK.toml entry for {id}: {reason}")]
    MalformedEntry { id: String, reason: String },
}

/// Raw TOML shapes. `deny_unknown_fields` on both: the lock is machine-written, so any key the
/// generator did not write is a hand edit or a merge accident worth failing loudly on.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLock {
    #[serde(default)]
    rules: BTreeMap<String, RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    version: i64,
    hash: String,
}

/// Whether `hash` is exactly `sha256:` followed by 64 lowercase hex digits.
fn is_well_formed_hash(hash: &str) -> bool {
    hash.strip_prefix(RULE_HASH_PREFIX).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

const LOCK_HEADER: &str = "\
# Rule version lock. GENERATED by `camerata corpus lock`; do not edit by hand.
# Design: docs/design/2026-10-20_corpus-staleness-check.md (Amendment 2026-10-21).
#
# Each rule's `version` and the content hash it had when the lock was written. The rules-crate
# lock gate fails when a rule's content changed but its `version` did not, so a change to a
# rule cannot ship without a revision bump. The hash covers the whole rule (every field,
# options included) and excludes `version` and `last_reviewed`.
#
# To change a rule: edit it, bump its `version`, run `camerata corpus lock`, commit both. The
# command refuses a rule that changed without a bump. Never edit this file to make a failing
# gate pass.
";

impl CorpusLock {
    /// Parse a lock from TOML text. Every entry must have `version >= 1` and a well-formed
    /// `sha256:<64 hex>` hash.
    pub fn parse(text: &str) -> Result<Self, LockError> {
        let raw: RawLock = toml::from_str(text)?;
        let mut rules = BTreeMap::new();
        for (id, entry) in raw.rules {
            let version = u32::try_from(entry.version)
                .ok()
                .filter(|v| *v >= 1)
                .ok_or_else(|| LockError::MalformedEntry {
                    id: id.clone(),
                    reason: format!("version {} (must be an integer >= 1)", entry.version),
                })?;
            if !is_well_formed_hash(&entry.hash) {
                return Err(LockError::MalformedEntry {
                    id,
                    reason: format!(
                        "hash {:?} (must be `{RULE_HASH_PREFIX}` and 64 lowercase hex digits)",
                        entry.hash
                    ),
                });
            }
            rules.insert(
                id,
                LockEntry {
                    version,
                    hash: entry.hash,
                },
            );
        }
        Ok(CorpusLock { rules })
    }

    /// Read and parse the lock at `path`. A missing file is an error (the gate must not pass
    /// because the lock vanished); callers that treat absence as "no lock yet" check for the
    /// file first.
    pub fn load(path: &Path) -> Result<Self, LockError> {
        let text = std::fs::read_to_string(path).map_err(|source| LockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// The canonical file text: a fixed header, then one sorted line per rule. Deterministic, so
    /// regenerating an unchanged corpus produces a byte-identical file.
    pub fn render(&self) -> String {
        let mut out = String::from(LOCK_HEADER);
        out.push_str("\n[rules]\n");
        for (id, entry) in &self.rules {
            out.push_str(&format!(
                "\"{id}\" = {{ version = {}, hash = \"{}\" }}\n",
                entry.version, entry.hash
            ));
        }
        out
    }

    /// Build a lock from the corpus as it stands, with no regard for any existing lock. Fails
    /// with every rule that cannot be locked (no `version`, or an id used by two rules). Prefer
    /// [`CorpusLock::regenerate`], which also refuses unbumped changes.
    pub fn from_corpus(set: &RuleSet) -> Result<Self, Vec<LockProblem>> {
        let mut problems = Vec::new();
        let mut rules = BTreeMap::new();
        for rule in set.iter() {
            let Some(version) = rule.version else {
                problems.push(LockProblem::Unversioned {
                    id: rule.id.0.clone(),
                });
                continue;
            };
            let entry = LockEntry {
                version,
                hash: rule.content_hash(),
            };
            if rules.insert(rule.id.0.clone(), entry).is_some() {
                problems.push(LockProblem::DuplicateId {
                    id: rule.id.0.clone(),
                });
            }
        }
        if problems.is_empty() {
            Ok(CorpusLock { rules })
        } else {
            problems.sort_by(|a, b| a.id().cmp(b.id()));
            problems.dedup();
            Err(problems)
        }
    }

    /// The engine of `camerata corpus lock`: the lock for the corpus as it stands, or the
    /// problems that make writing it unsafe. Refuses when a rule's hash changed without a
    /// version bump ([`LockProblem::ChangedWithoutBump`]), when a version went backwards, when
    /// a rule has no version, or when an id is duplicated: exactly the
    /// [`LockProblem::blocks_regeneration`] set. New rules, bumped rules and removed rules are
    /// what regeneration is for and are allowed.
    pub fn regenerate(
        set: &RuleSet,
        existing: Option<&CorpusLock>,
    ) -> Result<Self, Vec<LockProblem>> {
        let empty = CorpusLock::default();
        let blocking: Vec<LockProblem> = check_lock(set, existing.unwrap_or(&empty))
            .into_iter()
            .filter(LockProblem::blocks_regeneration)
            .collect();
        if !blocking.is_empty() {
            return Err(blocking);
        }
        Self::from_corpus(set)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// The gate
// ────────────────────────────────────────────────────────────────────────────

/// One way the corpus and the lock disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockProblem {
    /// The rule declares no `version`, so it cannot be locked.
    Unversioned { id: String },
    /// Two rules share an id; the lock is keyed by id and would silently cover only one.
    DuplicateId { id: String },
    /// The rule's content hash differs from the lock while its version is the locked one: it
    /// changed and nobody bumped `version`.
    ChangedWithoutBump { id: String, version: u32 },
    /// The rule's version is below the locked one. Versions only move forward.
    VersionRegressed {
        id: String,
        locked: u32,
        current: u32,
    },
    /// The rule is in the corpus but not in the lock.
    Unlocked { id: String },
    /// The rule's version is above the locked one: it was bumped but the lock was not
    /// regenerated.
    StaleVersion {
        id: String,
        locked: u32,
        current: u32,
    },
    /// The lock names a rule that is no longer in the corpus.
    Orphaned { id: String },
}

impl LockProblem {
    /// The rule id this problem is about.
    pub fn id(&self) -> &str {
        match self {
            LockProblem::Unversioned { id }
            | LockProblem::DuplicateId { id }
            | LockProblem::ChangedWithoutBump { id, .. }
            | LockProblem::VersionRegressed { id, .. }
            | LockProblem::Unlocked { id }
            | LockProblem::StaleVersion { id, .. }
            | LockProblem::Orphaned { id } => id,
        }
    }

    /// Whether `camerata corpus lock` must REFUSE to write over this problem. The ones that are
    /// not blocking are the ordinary effects of editing the corpus (a new rule, a bump, a
    /// removal) that regeneration exists to record; the blocking ones are a missed bump or a
    /// malformed corpus, which regeneration must never paper over.
    pub fn blocks_regeneration(&self) -> bool {
        match self {
            LockProblem::Unversioned { .. }
            | LockProblem::DuplicateId { .. }
            | LockProblem::ChangedWithoutBump { .. }
            | LockProblem::VersionRegressed { .. } => true,
            LockProblem::Unlocked { .. }
            | LockProblem::StaleVersion { .. }
            | LockProblem::Orphaned { .. } => false,
        }
    }
}

impl fmt::Display for LockProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockProblem::Unversioned { id } => write!(
                f,
                "{id}: no `version` declared; add `version = 1` (a new rule) or the next \
                 revision number"
            ),
            LockProblem::DuplicateId { id } => write!(
                f,
                "{id}: two rules share this id; the lock is keyed by id, so one would go \
                 unlocked. Rename one (ids are forever, so pick the newer rule)"
            ),
            LockProblem::ChangedWithoutBump { id, version } => write!(
                f,
                "{id}: rule changed but `version` is still {version}; bump `version`, then run \
                 `camerata corpus lock`"
            ),
            LockProblem::VersionRegressed {
                id,
                locked,
                current,
            } => write!(
                f,
                "{id}: `version` is {current} but the lock records {locked}; versions only move \
                 forward. Restore `version` to at least {locked}"
            ),
            LockProblem::Unlocked { id } => write!(
                f,
                "{id}: not in CORPUS_LOCK.toml; run `camerata corpus lock`"
            ),
            LockProblem::StaleVersion {
                id,
                locked,
                current,
            } => write!(
                f,
                "{id}: `version` is {current} but the lock records {locked}; run `camerata \
                 corpus lock`"
            ),
            LockProblem::Orphaned { id } => write!(
                f,
                "{id}: CORPUS_LOCK.toml names a rule that no longer exists; run `camerata \
                 corpus lock`"
            ),
        }
    }
}

/// The lock gate: every disagreement between `set` and `lock`, sorted by rule id; empty means
/// consistent. Pure.
///
/// Per rule, in order: no `version` is [`LockProblem::Unversioned`]; absent from the lock is
/// [`LockProblem::Unlocked`]; a version below the locked one is
/// [`LockProblem::VersionRegressed`]; a version above it is [`LockProblem::StaleVersion`] (the
/// lock must be regenerated, whether or not the content changed); the SAME version with a
/// different hash is [`LockProblem::ChangedWithoutBump`]. A lock entry with no rule is
/// [`LockProblem::Orphaned`].
pub fn check_lock(set: &RuleSet, lock: &CorpusLock) -> Vec<LockProblem> {
    let mut problems = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();

    for rule in set.iter() {
        let id = rule.id.0.as_str();
        if !seen.insert(id) {
            problems.push(LockProblem::DuplicateId { id: id.to_string() });
            continue;
        }
        let Some(version) = rule.version else {
            problems.push(LockProblem::Unversioned { id: id.to_string() });
            continue;
        };
        let Some(entry) = lock.rules.get(id) else {
            problems.push(LockProblem::Unlocked { id: id.to_string() });
            continue;
        };
        if version < entry.version {
            problems.push(LockProblem::VersionRegressed {
                id: id.to_string(),
                locked: entry.version,
                current: version,
            });
        } else if version > entry.version {
            problems.push(LockProblem::StaleVersion {
                id: id.to_string(),
                locked: entry.version,
                current: version,
            });
        } else if rule.content_hash() != entry.hash {
            problems.push(LockProblem::ChangedWithoutBump {
                id: id.to_string(),
                version,
            });
        }
    }

    for id in lock.rules.keys() {
        if !seen.contains(id.as_str()) {
            problems.push(LockProblem::Orphaned { id: id.clone() });
        }
    }

    problems.sort_by(|a, b| a.id().cmp(b.id()));
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the digest framing byte for byte against an independent oracle (Python `hashlib`
    /// over `"camerata-corpus-digest/v1\n"` plus one `id<TAB>version<TAB>hash<LF>` line per
    /// entry, sorted by id, `-` for no version). If this fails, every corpus version a client
    /// has been shown has changed meaning: a deliberate format migration (a new `/vN` tag),
    /// never an incidental edit.
    #[test]
    fn corpus_digest_matches_the_independent_oracle() {
        let entry = |id: &str, version: Option<u32>, fill: char| {
            (
                id.to_string(),
                version,
                format!("sha256:{}", fill.to_string().repeat(64)),
            )
        };
        // Supplied out of order on purpose.
        let digest = corpus_digest(vec![
            entry("B-2", Some(3), 'b'),
            entry("A-1", Some(1), 'a'),
            entry("C-3", None, 'c'),
        ]);
        assert_eq!(
            digest,
            "sha256:278bb359b6bc0805123daf3293e10429bb8618555423e29216d2212e92b2013f"
        );
        assert_eq!(
            corpus_digest(Vec::new()),
            "sha256:433a69d36196f6a190713bd27aeca970794d772708d158c9c82ba5c1c169f1fd"
        );
    }
}
