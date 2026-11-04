//! The seeded review data set: a whole run's worth of review views built from one seed.
//!
//! Everything here is a pure function of [`Params`]: the same params give byte-identical output.
//! The generator uses its own small PRNG (splitmix64) so no dependency can change the sequence.
//! The 30,000-site set is generated on demand and never stored.

use std::collections::BTreeMap;

use camerata_api_types::audit::FindingKind;
use camerata_api_types::decisions::{ClientAck, DecisionState, Scope};
use camerata_api_types::edits::EditField;
use camerata_api_types::review::{
    DisagreementField, EscalationBudget, EscalationCandidate, EscalationReason, EscalationState,
    OutlierReason, ReviewLevel, Stage2Disagreement, ENGINE_VERIFIED, PERSONALLY_REVIEWED,
    REVIEWED_AS_PATTERN_PREFIX,
};
use camerata_api_types::review_view::{
    BreadcrumbItem, CategoryAllocation, CorrectionRef, DecisionView, DuplicateView, EditView,
    EngineCheckCard, EngineCheckError, EngineCheckLane, EngineCheckSettingsView,
    EngineCheckVerdict, EscalationSettingsView, EscalationStatus, ExportGateView, FlaggedByView,
    HeldRow, HoldReason, LaneCounts, Marker, MergedIntoView, MovedFromView, NeededMeSummary,
    NewSinceDecisionView, NodeCountsView, NodeDecisionGap, NodeDecisionSummary, OutlierView,
    PlacementView, QueueRow, QueueView, ReviewCounts, ReviewSettingsView, ReviewShellView,
    SampleStratum, SettingsLimits, SiteRow, SitesPage, Stage1View, Stage2View, StageModelsView,
    StaleSummary, StratumRate, SummarySource, TreeLevel, TreeNodeKind, TreeNodeView, TreeView,
    ESCALATION_FLOOR_MAX, ESCALATION_RATE_MAX, QUEUE_BELOW_PAGE_SIZE, SAMPLE_SIZE_MAX,
    SAMPLE_SIZE_SUGGESTED, SITES_PAGE_SIZE, SMALL_CATEGORY_DRAWN,
};
use serde::Serialize;

// ── The generator ─────────────────────────────────────────────────────────────────────────

/// splitmix64: tiny, fast and fixed forever.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`; `n` must be above zero.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// Fisher-Yates over `0..n`.
    fn permutation(&mut self, n: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
        v
    }
}

/// A real site key: `repo NUL path NUL fingerprint NUL ordinal`, the fingerprint 16 hex digits.
/// Readable fixture keys would hide the bug class where a screen puts a key in an element id, an
/// href or a `data-` attribute, so the generator uses the real shape.
fn site_key(repo: &str, path: &str, rule: &str, line: usize, ordinal: usize) -> String {
    // FNV-1a over the pieces that identify a finding, fixed forever like the PRNG.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in format!("{rule}\0{path}\0{line}").bytes() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{repo}\0{path}\0{h:016x}\0{ordinal}")
}

/// The sites-page cursor: an opaque hex blob, as the server's is.
fn hex_encode(key: &str) -> String {
    key.bytes().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(cursor: &str) -> Option<String> {
    if !cursor.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..cursor.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(cursor.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// What a data set contains. Counts are exact: a count of 12 outliers gives 12 outliers.
#[derive(Debug, Clone)]
pub struct Params {
    pub seed: u64,
    pub categories: usize,
    pub rules_per_category: usize,
    pub subgroups_per_rule: usize,
    /// When set, the tree has exactly this many sub-groups: categories and rules are filled in
    /// order (`rules_per_category` rules of `subgroups_per_rule` sub-groups each) until they
    /// run out, the last rule and category possibly part full. Every sub-group then holds at
    /// least one site. `None` fills `categories` x `rules_per_category` x `subgroups_per_rule`.
    pub total_subgroups: Option<usize>,
    /// Sites forced into the first sub-group of rules spread across the tree, one entry per big
    /// sub-group (only with `total_subgroups`). A real run is skewed: a few sub-groups hold
    /// thousands of sites and most hold a dozen.
    pub big_subgroups: Vec<usize>,
    /// Sites in all, not counting thrown-out rows.
    pub sites: usize,
    pub counted_lines: usize,
    pub rate_per_kloc: f32,
    pub floor: usize,
    pub candidates: usize,
    pub over_budget_reasons: Vec<EscalationReason>,
    pub outliers: usize,
    /// Held rows other than merged ones (evidence unverified, junk, restored, Engine Check No).
    pub held: usize,
    pub merged_without_decision: usize,
    pub edited: usize,
    pub stale_edits: usize,
    pub stale_decisions: usize,
    pub moved: usize,
    pub duplicates: usize,
    pub decided_subgroups: usize,
    /// Sub-groups of which every member is decided by one group decision (no outlier, no held
    /// row, no new member), so the tree draws its green check.
    pub fully_decided_subgroups: usize,
    pub decided_rules: usize,
    /// Covered sites whose inherited record is missing: decided sub-group members with no
    /// per-site record. The only thing that feeds the export gate's `node_decisions_missing_members`.
    pub decision_gaps: usize,
    pub new_since_decision: usize,
    pub site_decisions: usize,
    pub thrown_out: usize,
    /// Of every hundred sites, how many stage 1 did not read.
    pub unread_percent: usize,
    pub sample_size: usize,
    pub group_reviewed: bool,
    /// Findings the reviewer added by hand.
    pub added: usize,
    /// What the Engine Check cards carry.
    pub engine_mode: EngineMode,
}

/// Which verdicts the Engine Check cards carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineMode {
    /// No card has a verdict.
    Pending,
    /// The first card says No and nothing corrects it: the row is held.
    NoWithoutCorrection,
    /// The first card says No and a decision corrects it.
    NoCorrected,
    /// Every card has a verdict; each No is corrected.
    AllVerdicts,
}

/// The reasons that may exceed the budget (design A4 and D5): the server's default set.
pub const OVER_BUDGET_REASONS: [EscalationReason; 5] = [
    EscalationReason::UnauthenticatedReach,
    EscalationReason::SecretExposure,
    EscalationReason::DataLoss,
    EscalationReason::CrossTenant,
    EscalationReason::EvidenceUnverified,
];

impl Params {
    /// The working set most states use: 600 sites in 4 categories, every marker present.
    pub fn base(seed: u64) -> Self {
        Self {
            seed,
            categories: 4,
            rules_per_category: 3,
            subgroups_per_rule: 3,
            total_subgroups: None,
            big_subgroups: Vec::new(),
            sites: 600,
            counted_lines: 12_000,
            rate_per_kloc: 1.0,
            floor: 20,
            candidates: 48,
            over_budget_reasons: OVER_BUDGET_REASONS.to_vec(),
            outliers: 14,
            held: 8,
            merged_without_decision: 3,
            edited: 9,
            stale_edits: 4,
            stale_decisions: 4,
            moved: 6,
            duplicates: 7,
            decided_subgroups: 6,
            fully_decided_subgroups: 2,
            decided_rules: 1,
            decision_gaps: 2,
            new_since_decision: 5,
            site_decisions: 60,
            thrown_out: 6,
            unread_percent: 12,
            sample_size: 50,
            group_reviewed: true,
            added: 3,
            engine_mode: EngineMode::Pending,
        }
    }

    /// The scale set: 30,000 sites in the shape the server's measurement fixture has (design
    /// A14): 58 categories of up to 8 rules of 5 sub-groups, 2,308 sub-groups and 2,828 nodes
    /// in all, with a few large sub-groups so a node needs more than one page of sites.
    pub fn scale_30k(seed: u64) -> Self {
        Self {
            seed,
            categories: 58,
            rules_per_category: 8,
            subgroups_per_rule: 5,
            total_subgroups: Some(2_308),
            big_subgroups: vec![1_340, 820, 560],
            sites: 30_000,
            counted_lines: 480_000,
            rate_per_kloc: 1.0,
            floor: 20,
            candidates: 700,
            over_budget_reasons: OVER_BUDGET_REASONS.to_vec(),
            outliers: 450,
            held: 90,
            merged_without_decision: 12,
            edited: 120,
            stale_edits: 20,
            stale_decisions: 30,
            moved: 60,
            duplicates: 150,
            decided_subgroups: 140,
            fully_decided_subgroups: 20,
            decided_rules: 6,
            decision_gaps: 3,
            new_since_decision: 90,
            site_decisions: 900,
            thrown_out: 40,
            unread_percent: 10,
            sample_size: 50,
            group_reviewed: true,
            added: 5,
            engine_mode: EngineMode::Pending,
        }
    }
}

// ── Word pools (no dashes, no serial commas) ──────────────────────────────────────────────

const CATEGORIES: [(&str, &str, &str); 12] = [
    ("authorization", "Authorization", "AUTH"),
    ("secrets", "Secrets and credentials", "SEC"),
    ("injection", "Injection", "INJ"),
    ("input-validation", "Input validation", "VAL"),
    ("data-access", "Data access", "DAT"),
    ("error-handling", "Error handling", "ERR"),
    ("logging", "Logging and audit", "LOG"),
    ("crypto", "Cryptography", "CRY"),
    ("dependencies", "Dependencies", "DEP"),
    ("configuration", "Configuration", "CFG"),
    ("api-design", "API design", "API"),
    ("testing", "Testing", "TST"),
];

const RULE_TOPICS: [&str; 20] = [
    "Row level security missing",
    "Route without an auth guard",
    "Hardcoded credential",
    "Token written to the log",
    "Unparameterised query",
    "Shell command built from input",
    "Unchecked request body",
    "Missing length limit",
    "Query without a tenant filter",
    "Error swallowed silently",
    "Stack trace returned to the client",
    "Audit entry missing on write",
    "Weak hash for passwords",
    "Static initialisation vector",
    "Dependency with a known advisory",
    "Unpinned dependency range",
    "Debug flag left on",
    "Wildcard CORS origin",
    "Unversioned endpoint",
    "Test that asserts nothing",
];

const SUBGROUP_LABELS: [&str; 10] = [
    "Handlers that skip the shared guard",
    "Policies absent on a table",
    "Inputs read without a schema",
    "Helpers that wrap the raw call",
    "Scripts and one off tools",
    "Legacy routes kept for a client",
    "Background jobs",
    "Admin only pages",
    "Generated clients",
    "Fixtures and seeds",
];

const SUBGROUP_CAUSES: [&str; 6] = [
    "Each one calls the database directly instead of through the access layer.",
    "Each one reads a value from the request and passes it on unchanged.",
    "Each one was copied from the same template file.",
    "Each one sits outside the folder the shared middleware covers.",
    "Each one predates the current guard and was never migrated.",
    "Each one is reached only from a trusted internal caller.",
];

const DIRS: [&str; 8] = [
    "web/src/routes",
    "api/src/handlers",
    "api/src/jobs",
    "services/billing/src",
    "services/search/src",
    "lib/db",
    "web/src/components",
    "tools/scripts",
];

const STEMS: [&str; 12] = [
    "orders", "users", "invoices", "auth", "teams", "reports", "export", "webhooks", "sessions",
    "settings", "uploads", "search",
];

const DETAILS: [&str; 6] = [
    "The handler reads the value from the request and uses it without a check.",
    "Nothing between the route and the query confirms the caller may see this row.",
    "The same pattern appears in the neighbouring handlers and was fixed there.",
    "The value reaches the database call exactly as the client sent it.",
    "The call succeeds for any signed in user whatever their team.",
    "The code path is reachable from the public router.",
];

const FIXES: [&str; 5] = [
    "Wrap the call in the shared guard and pass the caller through.",
    "Add the tenant filter to the query and cover it with a test.",
    "Validate the body against the schema before it is used.",
    "Replace the literal with a read from the secret store.",
    "Return a generic message and log the detail on the server.",
];

const WHYS: [&str; 5] = [
    "Reachable without a session and touches customer data.",
    "Only site in the group that writes to the table.",
    "Stage 1 called this low but the path is public.",
    "The rule fits poorly: this call is already behind a gateway.",
    "Needs the caller file to be sure.",
];

const OUTLIER_WHYS: [&str; 4] = [
    "Reads a different object from the other sites in this sub-group.",
    "The only site in its file; the others sit together.",
    "Lives in test code; the rest are production handlers.",
    "Could be intentional: the route is documented as public.",
];

const REPOS: [&str; 2] = ["acme-web", "acme-api"];

fn severity_rank(s: &str) -> usize {
    match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

fn timestamp(i: usize) -> String {
    format!(
        "2026-11-{:02}T{:02}:{:02}:00Z",
        4 + i % 3,
        9 + i % 8,
        i % 60
    )
}

// ── The layout of the tree ────────────────────────────────────────────────────────────────

struct RuleDef {
    id: String,
    label: String,
    node: String,
    category: usize,
    weight: usize,
    subs: Vec<SubDef>,
}

struct SubDef {
    node: String,
    label: String,
    cause: String,
    weight: usize,
}

struct Layout {
    rules: Vec<RuleDef>,
    total_weight: usize,
}

impl Layout {
    fn new(p: &Params, rng: &mut Rng) -> Self {
        let mut rules = Vec::new();
        // Sub-groups still to place when the tree has a fixed number of them.
        let mut left = p.total_subgroups.unwrap_or(usize::MAX);
        'categories: for c in 0..p.categories {
            let prefix = category_parts(c).2;
            for r in 0..p.rules_per_category {
                let in_this_rule = p.subgroups_per_rule.min(left);
                if in_this_rule == 0 {
                    break 'categories;
                }
                left = left.saturating_sub(in_this_rule);
                let id = format!("{prefix}-{}", r + 1);
                let weight = 1 + rng.below(9);
                let subs = (0..in_this_rule)
                    .map(|s| SubDef {
                        node: format!("sub:{id}/{}", s + 1),
                        label: format!(
                            "{} ({})",
                            SUBGROUP_LABELS[(c + r + s) % SUBGROUP_LABELS.len()],
                            s + 1
                        ),
                        cause: SUBGROUP_CAUSES[(c * 3 + r + s) % SUBGROUP_CAUSES.len()].to_string(),
                        weight: 1 + rng.below(4),
                    })
                    .collect();
                rules.push(RuleDef {
                    label: RULE_TOPICS[(c * 7 + r) % RULE_TOPICS.len()].to_string(),
                    node: format!("rule:{id}"),
                    id,
                    category: c,
                    weight,
                    subs,
                });
            }
        }
        let total_weight = rules.iter().map(|r| r.weight).sum();
        Self {
            rules,
            total_weight,
        }
    }

    /// How many sub-groups the layout has.
    fn sub_count(&self) -> usize {
        self.rules.iter().map(|r| r.subs.len()).sum()
    }

    /// The `n`th sub-group in tree order, as (rule, sub).
    fn nth_sub(&self, n: usize) -> Option<(usize, usize)> {
        let mut left = n;
        for (ri, r) in self.rules.iter().enumerate() {
            if left < r.subs.len() {
                return Some((ri, left));
            }
            left -= r.subs.len();
        }
        None
    }

    /// Where site `i` goes. With a fixed number of sub-groups the first sites cover each
    /// sub-group once (so none is empty) and the next ones fill the big sub-groups; the rest,
    /// and every site of a set without a fixed shape, are picked by weight.
    fn place(&self, i: usize, rng: &mut Rng, p: &Params) -> (usize, usize) {
        if p.total_subgroups.is_some() {
            let covered = self.sub_count();
            if let Some(at) = self.nth_sub(i).filter(|_| i < covered) {
                return at;
            }
            let mut j = i.saturating_sub(covered);
            for (k, &size) in p.big_subgroups.iter().enumerate() {
                if j < size {
                    let rule = (k * 40) % self.rules.len().max(1);
                    return (rule, 0);
                }
                j -= size;
            }
        }
        self.pick(rng)
    }

    /// Pick a rule then a sub-group, weighted.
    fn pick(&self, rng: &mut Rng) -> (usize, usize) {
        let mut roll = rng.below(self.total_weight);
        let mut ri = 0;
        for (i, r) in self.rules.iter().enumerate() {
            if roll < r.weight {
                ri = i;
                break;
            }
            roll -= r.weight;
        }
        let rule = &self.rules[ri];
        let sw: usize = rule.subs.iter().map(|s| s.weight).sum();
        let mut roll = rng.below(sw);
        for (si, s) in rule.subs.iter().enumerate() {
            if roll < s.weight {
                return (ri, si);
            }
            roll -= s.weight;
        }
        (ri, 0)
    }
}

/// The slug, label and rule-id prefix of category `c`. Past the twelve names the pool has, a
/// name repeats with a number (`secrets-2`, `Secrets and credentials 2`, `SEC2`), so every
/// category and rule id is unique at any size.
fn category_parts(c: usize) -> (String, String, String) {
    let (slug, label, prefix) = CATEGORIES[c % CATEGORIES.len()];
    let round = c / CATEGORIES.len();
    if round == 0 {
        (slug.to_string(), label.to_string(), prefix.to_string())
    } else {
        (
            format!("{slug}-{}", round + 1),
            format!("{label} {}", round + 1),
            format!("{prefix}{}", round + 1),
        )
    }
}

fn cat_node(c: usize) -> String {
    format!("cat:{}", category_parts(c).0)
}

const THROWN_CAT: &str = "cat:thrown-out";
const THROWN_RULE: &str = "rule:THROWN-1";
const THROWN_SUB: &str = "sub:THROWN-1/1";

/// A review level read back from its wire form. The server computes the level in one place
/// (`apply_overlay`) and a source scan keeps it so; a fixture decodes what the server would have
/// written instead of building a level itself.
fn level(wire: &str) -> Option<ReviewLevel> {
    ReviewLevel::parse(wire)
}

// ── Rows ──────────────────────────────────────────────────────────────────────────────────

fn make_row(i: usize, layout: &Layout, rng: &mut Rng, p: &Params) -> SiteRow {
    let (ri, si) = layout.place(i, rng, p);
    let rule = &layout.rules[ri];
    let sub = &rule.subs[si];
    let cat_label = category_parts(rule.category).1;
    let severity = match rng.below(100) {
        0..=2 => "critical",
        3..=19 => "high",
        20..=64 => "medium",
        _ => "low",
    };
    let repo = REPOS[rng.below(REPOS.len())];
    let path = format!(
        "{}/{}{}.ts",
        DIRS[rng.below(DIRS.len())],
        STEMS[rng.below(STEMS.len())],
        i % 97
    );
    let line = 5 + rng.below(480);
    let snippet = format!("db.query(req.body.q{i}) // site {i}");
    let unread = rng.below(100) < p.unread_percent;
    let real = if unread {
        String::new()
    } else {
        ["yes", "yes", "yes", "no", "unclear"][rng.below(5)].to_string()
    };
    let stage1 = Stage1View {
        severity: severity.to_string(),
        real: real.clone(),
        effort: (!unread).then(|| ["low", "medium", "high"][rng.below(3)].to_string()),
        confidence: (!unread).then(|| ["high", "medium", "low"][rng.below(3)].to_string()),
        reason: if unread {
            String::new()
        } else {
            DETAILS[rng.below(DETAILS.len())].to_string()
        },
    };
    let stage2 = p.group_reviewed.then(|| Stage2View {
        state: EscalationState::NotACandidate,
        reason: None,
        why: String::new(),
        stage2_rank: None,
        global_rank: None,
        over_budget_reason: None,
        outlier_reason: None,
        outlier_why: String::new(),
        subgroup_label: sub.label.clone(),
        subgroup_cause: sub.cause.clone(),
        disagreements: Vec::new(),
    });
    SiteRow {
        site_key: site_key(repo, &path, &rule.id, line, i),
        repo: repo.to_string(),
        path,
        line,
        rule_id: rule.id.clone(),
        severity: severity.to_string(),
        kind: Some(FindingKind::Defect),
        category: Some(cat_label),
        headline: format!("{} at line {line}", rule.label),
        detail: DETAILS[rng.below(DETAILS.len())].to_string(),
        fix: FIXES[rng.below(FIXES.len())].to_string(),
        fix_type: if rng.below(3) == 0 {
            "Tailored fix"
        } else {
            "Standard fix"
        }
        .to_string(),
        snippet,
        group_path: vec![cat_node(rule.category), rule.node.clone(), sub.node.clone()],
        group_primary: false,
        stage1,
        stage2,
        markers: Vec::new(),
        decision: None,
        edits: Vec::new(),
        stale_decision: None,
        new_since_decision: None,
        moved_from: None,
        flagged_by: Vec::new(),
        possible_duplicate_of: None,
        merged_into: None,
        held: None,
        review_level: None,
        added_by_reviewer: false,
        thrown_out_reason: None,
        review_flags: Vec::new(),
    }
}

fn make_thrown_row(n: usize, rng: &mut Rng, sites: usize) -> SiteRow {
    let i = sites + n;
    let path = format!("api/src/legacy/old{n}.ts");
    let line = 10 + rng.below(200);
    SiteRow {
        site_key: site_key("acme-api", &path, "THROWN-1", line, i),
        repo: "acme-api".to_string(),
        path,
        line,
        rule_id: "THROWN-1".to_string(),
        severity: "low".to_string(),
        kind: Some(FindingKind::Defect),
        category: Some("Thrown out by the engine".to_string()),
        headline: "Placeholder text where a finding should be".to_string(),
        detail: "The engine kept no usable text for this row.".to_string(),
        fix: String::new(),
        fix_type: "Standard fix".to_string(),
        snippet: format!("// thrown {n}"),
        group_path: vec![
            THROWN_CAT.to_string(),
            THROWN_RULE.to_string(),
            THROWN_SUB.to_string(),
        ],
        group_primary: false,
        stage1: Stage1View {
            severity: "low".to_string(),
            ..Stage1View::default()
        },
        stage2: None,
        markers: Vec::new(),
        decision: None,
        edits: Vec::new(),
        stale_decision: None,
        new_since_decision: None,
        moved_from: None,
        flagged_by: Vec::new(),
        possible_duplicate_of: None,
        merged_into: None,
        held: None,
        review_level: None,
        added_by_reviewer: false,
        thrown_out_reason: Some("junk text: a placeholder where the finding should be".to_string()),
        review_flags: Vec::new(),
    }
}

/// The markers a row has, in [`Marker::PRECEDENCE`] order, read from its fields.
pub fn markers_for(row: &SiteRow) -> Vec<Marker> {
    let st = row.stage2.as_ref();
    let escalated = st.is_some_and(|s| s.state == EscalationState::Escalated);
    let below = st.is_some_and(|s| s.state == EscalationState::BelowTheLine);
    let over = escalated && st.is_some_and(|s| s.over_budget_reason.is_some());
    let outlier = st.is_some_and(|s| s.outlier_reason.is_some());
    let stale = row.stale_decision.is_some() || row.edits.iter().any(|e| e.stale);
    let edited = row.edits.iter().any(|e| !e.stale);
    Marker::PRECEDENCE
        .iter()
        .copied()
        .filter(|m| match m {
            Marker::Held => row.held.is_some(),
            Marker::Escalated => escalated,
            Marker::Candidate => below,
            Marker::OverBudget => over,
            Marker::Outlier => outlier,
            Marker::NewSinceDecision => row.new_since_decision.is_some(),
            Marker::Stale => stale,
            Marker::Moved => row.moved_from.is_some(),
            Marker::PossibleDuplicate => row.possible_duplicate_of.is_some(),
            Marker::Edited => edited,
        })
        .collect()
}

// ── The data set ──────────────────────────────────────────────────────────────────────────

/// One decided node: who it covers and who it left out.
struct NodeDecisionRecord {
    node: String,
    id: String,
    state: DecisionState,
    reason: String,
    covered: usize,
    left_out: usize,
}

/// Every view of one run, ready to be read through a `ReviewSource`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewDataSet {
    pub shell: ReviewShellView,
    pub tree: TreeView,
    /// Every site, in the fixed order (severity, path, line, key).
    pub rows: Vec<SiteRow>,
    /// Node id to the indexes of its rows (in `rows` order), for every level.
    pub node_rows: BTreeMap<String, Vec<usize>>,
    /// Every candidate in global rank order.
    pub candidates: Vec<EscalationCandidate>,
    /// Node id to its label, for breadcrumbs.
    pub node_labels: BTreeMap<String, String>,
    pub engine: EngineCheckLane,
    pub gate: ExportGateView,
    pub settings: ReviewSettingsView,
}

impl ReviewDataSet {
    /// The bytes the determinism test compares: the whole data set as JSON.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn row(&self, site_key: &str) -> Option<&SiteRow> {
        self.rows.iter().find(|r| r.site_key == site_key)
    }

    /// One node's sites, `limit` at a time (never above [`SITES_PAGE_SIZE`]), resuming after the
    /// row named by `cursor`. `None` when there is no such node.
    pub fn sites_page(&self, node: &str, cursor: Option<&str>, limit: usize) -> Option<SitesPage> {
        let members = self.node_rows.get(node)?;
        let start = cursor
            .and_then(hex_decode)
            .and_then(|c| members.iter().position(|&i| self.rows[i].site_key == c))
            .map_or(0, |p| p + 1);
        let end = (start + limit.clamp(1, SITES_PAGE_SIZE)).min(members.len());
        let rows: Vec<SiteRow> = members[start.min(end)..end]
            .iter()
            .map(|&i| self.rows[i].clone())
            .collect();
        let next_cursor = (end < members.len())
            .then(|| rows.last().map(|r| hex_encode(&r.site_key)))
            .flatten();
        Some(SitesPage {
            run_id: self.shell.run_id.clone(),
            node: node.to_string(),
            total: members.len(),
            offset: start.min(end),
            rows,
            next_cursor,
        })
    }

    fn queue_row(&self, c: &EscalationCandidate) -> Option<QueueRow> {
        let site = self.row(&c.key)?.clone();
        let node_path = site
            .group_path
            .iter()
            .map(|id| BreadcrumbItem {
                id: id.clone(),
                label: self
                    .node_labels
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.clone()),
            })
            .collect();
        Some(QueueRow {
            candidate: c.clone(),
            node_path,
            site,
        })
    }

    /// The queue (spec section 6). `show_below` is a view flag and changes nothing stored.
    pub fn queue(&self, show_below: bool, below_offset: usize, below_limit: usize) -> QueueView {
        let escalated: Vec<QueueRow> = self
            .candidates
            .iter()
            .filter(|c| c.escalated)
            .filter_map(|c| self.queue_row(c))
            .collect();
        let below_all: Vec<&EscalationCandidate> =
            self.candidates.iter().filter(|c| !c.escalated).collect();
        let below = if show_below {
            below_all
                .iter()
                .skip(below_offset)
                .take(below_limit.clamp(1, QUEUE_BELOW_PAGE_SIZE))
                .filter_map(|c| self.queue_row(c))
                .collect()
        } else {
            Vec::new()
        };
        let decided_n = escalated
            .iter()
            .filter(|q| q.site.decision.is_some())
            .count();
        let needed = escalated
            .iter()
            .filter(|q| q.site.decision.as_ref().and_then(|d| d.needed_me) == Some(true))
            .count();
        QueueView {
            run_id: self.shell.run_id.clone(),
            budget: self.shell.budget,
            over_budget_reasons: self.settings.escalation.over_budget_reasons.clone(),
            escalated,
            show_below,
            below,
            below_total: below_all.len(),
            below_offset: if show_below { below_offset } else { 0 },
            needed_me: NeededMeSummary {
                decided: decided_n,
                needed_me: needed,
                percent: (needed * 100)
                    .checked_div(decided_n)
                    .map_or(0, |p| u8::try_from(p).unwrap_or(100)),
            },
            group_reviewed: self.shell.group_reviewed,
        }
    }
}

/// Build the whole data set from `p`.
pub fn build(p: &Params) -> ReviewDataSet {
    let mut rng = Rng::new(p.seed);
    let layout = Layout::new(p, &mut rng);

    // Rows, sorted into the fixed order.
    let mut rows: Vec<SiteRow> = (0..p.sites)
        .map(|i| make_row(i, &layout, &mut rng, p))
        .collect();
    rows.extend((0..p.thrown_out).map(|n| make_thrown_row(n, &mut rng, p.sites)));
    rows.sort_by(|a, b| {
        (severity_rank(&a.severity), &a.path, a.line, &a.site_key).cmp(&(
            severity_rank(&b.severity),
            &b.path,
            b.line,
            &b.site_key,
        ))
    });
    // The first row of every sub-group in order is its primary.
    let mut seen_sub = std::collections::BTreeSet::new();
    for r in &mut rows {
        if r.thrown_out_reason.is_none() {
            r.group_primary = seen_sub.insert(r.group_path[2].clone());
        }
    }
    let normal: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.thrown_out_reason.is_none())
        .map(|(i, _)| i)
        .collect();

    // Which rows carry which feature: disjoint slices of one shuffled order.
    let order = rng.permutation(normal.len());
    let mut cursor = 0;
    let mut take = |n: usize| -> Vec<usize> {
        let end = (cursor + n).min(order.len());
        let out = order[cursor..end].iter().map(|&k| normal[k]).collect();
        cursor = end;
        out
    };
    let cand_rows = if p.group_reviewed {
        take(p.candidates)
    } else {
        take(0)
    };
    let outlier_rows = if p.group_reviewed {
        take(p.outliers)
    } else {
        take(0)
    };
    let held_rows = take(p.held);
    let merged_rows = take(p.merged_without_decision);
    let edited_rows = take(p.edited);
    let stale_edit_rows = take(p.stale_edits);
    let stale_decision_rows = take(p.stale_decisions);
    let moved_rows = take(p.moved);
    let dup_rows = take(p.duplicates);
    let added_rows = take(p.added);
    let mut special = vec![false; rows.len()];
    for group in [
        &cand_rows,
        &outlier_rows,
        &held_rows,
        &merged_rows,
        &edited_rows,
        &stale_edit_rows,
        &stale_decision_rows,
        &moved_rows,
        &dup_rows,
        &added_rows,
    ] {
        for &i in group {
            special[i] = true;
        }
    }
    // A few rows carry a second source ("Also flagged by"), on rows that are otherwise plain.
    for (n, &i) in normal.iter().enumerate().filter(|(n, _)| n % 40 == 7) {
        let _ = n;
        let path = format!("{}.bak", rows[i].path);
        rows[i].flagged_by = vec![FlaggedByView {
            rule_id: "SEMGREP-12".to_string(),
            tool: Some("semgrep".to_string()),
            site_key: site_key(&rows[i].repo, &path, "SEMGREP-12", rows[i].line, i),
            path,
            line: rows[i].line,
        }];
    }

    // Candidates: reasons, then the global ranking.
    let reasons = EscalationReason::ALL;
    for (n, &i) in cand_rows.iter().enumerate() {
        let reason = reasons[(n + rng.below(3)) % reasons.len()];
        let sev = rows[i].severity.clone();
        if let Some(s) = rows[i].stage2.as_mut() {
            s.reason = Some(reason);
            s.why = WHYS[rng.below(WHYS.len())].to_string();
            s.state = EscalationState::BelowTheLine;
            if n % 3 == 0 {
                s.disagreements.push(Stage2Disagreement {
                    field: DisagreementField::Severity,
                    stage1: sev,
                    mine: "high".to_string(),
                    why: "The path is public.".to_string(),
                });
            }
        }
    }
    let mut ranked: Vec<usize> = cand_rows.clone();
    ranked.sort_by(|&a, &b| {
        let key = |i: usize| {
            let reason = rows[i].stage2.as_ref().and_then(|s| s.reason);
            let prio = reason
                .and_then(|r| reasons.iter().position(|x| *x == r))
                .unwrap_or(99);
            (
                severity_rank(&rows[i].severity),
                prio,
                rows[i].site_key.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
    let mut budget = EscalationBudget::compute(p.rate_per_kloc, p.floor, p.counted_lines);
    let mut candidates = Vec::new();
    let mut per_rule_rank: BTreeMap<String, u32> = BTreeMap::new();
    let mut taken = 0usize;
    for (rank0, &i) in ranked.iter().enumerate() {
        let reason = rows[i]
            .stage2
            .as_ref()
            .and_then(|s| s.reason)
            .unwrap_or(EscalationReason::NeedsMoreCode);
        let (escalated, over) = if taken < budget.budget {
            (true, None)
        } else if p.over_budget_reasons.contains(&reason) {
            (true, Some(reason))
        } else {
            (false, None)
        };
        if escalated {
            taken += 1;
        }
        let rule_node = rows[i].group_path[1].clone();
        let slot = per_rule_rank.entry(rule_node.clone()).or_insert(0);
        *slot += 1;
        let global_rank = u32::try_from(rank0 + 1).unwrap_or(u32::MAX);
        let c = EscalationCandidate {
            key: rows[i].site_key.clone(),
            node_id: rule_node,
            reason,
            severity: rows[i].severity.clone(),
            stage2_rank: *slot,
            global_rank,
            escalated,
            over_budget_reason: over,
            why: rows[i]
                .stage2
                .as_ref()
                .map(|s| s.why.clone())
                .unwrap_or_default(),
        };
        if let Some(s) = rows[i].stage2.as_mut() {
            s.state = c.state();
            s.stage2_rank = Some(c.stage2_rank);
            s.global_rank = Some(c.global_rank);
            s.over_budget_reason = c.over_budget_reason;
        }
        candidates.push(c);
    }
    budget.escalated = candidates.iter().filter(|c| c.escalated).count();
    budget.over_budget = budget.escalated.saturating_sub(budget.budget);
    budget.candidates = candidates.len();

    // Outliers.
    let outlier_reasons = OutlierReason::ALL;
    for (n, &i) in outlier_rows.iter().enumerate() {
        if let Some(s) = rows[i].stage2.as_mut() {
            s.outlier_reason = Some(outlier_reasons[n % outlier_reasons.len()]);
            s.outlier_why = OUTLIER_WHYS[n % OUTLIER_WHYS.len()].to_string();
        }
    }
    // Held rows (the four reasons in turn) and merged rows without a decision.
    let hold_cycle = [
        HoldReason::EvidenceUnverified,
        HoldReason::JunkContent,
        HoldReason::RestoredWithoutDecision,
        HoldReason::EngineCheckNoWithoutCorrection,
    ];
    for (n, &i) in held_rows.iter().enumerate() {
        rows[i].held = Some(hold_cycle[n % hold_cycle.len()]);
    }
    for &i in &merged_rows {
        let target = format!("{}:{}", rows[normal[0]].path, rows[normal[0]].line);
        rows[i].held = Some(HoldReason::MergedWithoutDecision);
        rows[i].merged_into = Some(MergedIntoView {
            key: rows[normal[0]].site_key.clone(),
            label: target,
        });
    }
    // Edits (applied and stale).
    let fields = [
        EditField::Headline,
        EditField::Fix,
        EditField::Detail,
        EditField::Severity,
    ];
    for (n, &i) in edited_rows.iter().enumerate() {
        let field = fields[n % fields.len()];
        let (old, new) = edit_text(&rows[i], field);
        apply_edit_value(&mut rows[i], field, &new);
        rows[i].edits.push(EditView {
            id: format!("edit-{i}"),
            field,
            old,
            current_value: new.clone(),
            new,
            reason: "Wording was unclear to the client.".to_string(),
            by: "architect".to_string(),
            at: timestamp(i),
            stale: false,
            reapplied_from: None,
        });
        rows[i].review_level = level(PERSONALLY_REVIEWED);
    }
    for (n, &i) in stale_edit_rows.iter().enumerate() {
        let field = fields[n % 2];
        let (old, new) = edit_text(&rows[i], field);
        let now = format!("{old} (rewritten by the rerun)");
        rows[i].edits.push(EditView {
            id: format!("edit-{i}"),
            field,
            old,
            new,
            current_value: now,
            reason: "Reworded for the client.".to_string(),
            by: "architect".to_string(),
            at: timestamp(i),
            stale: true,
            reapplied_from: None,
        });
    }
    for &i in &stale_decision_rows {
        rows[i].stale_decision = Some(DecisionView {
            id: format!("dec-old-{i}"),
            state: DecisionState::Confirmed,
            reason: "Confirmed on the previous run.".to_string(),
            scope: Scope::Site,
            inherited_from: None,
            decided_by: "architect".to_string(),
            decided_at: timestamp(i),
            needed_me: None,
            confirmed_by_client: None,
        });
    }
    for &i in &moved_rows {
        let was = rows[i].line + 12;
        rows[i].moved_from = Some(MovedFromView {
            key: site_key(&rows[i].repo, &rows[i].path, &rows[i].rule_id, was, i),
            line: Some(was),
        });
    }
    for (n, &i) in dup_rows.iter().enumerate() {
        let other = &rows[normal[(n * 13) % normal.len()]];
        let dup = DuplicateView {
            key: other.site_key.clone(),
            hint: "same call, same file family".to_string(),
            rule_id: other.rule_id.clone(),
            path: other.path.clone(),
            line: other.line,
        };
        rows[i].possible_duplicate_of = Some(dup);
    }

    for &i in &added_rows {
        rows[i].added_by_reviewer = true;
        rows[i].review_level = level(PERSONALLY_REVIEWED);
    }

    // Node decisions: sub-groups first, then whole rules.
    let mut node_decisions: Vec<NodeDecisionRecord> = Vec::new();
    let mut sub_members: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut rule_members: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in &normal {
        sub_members
            .entry(rows[i].group_path[2].clone())
            .or_default()
            .push(i);
        rule_members
            .entry(rows[i].group_path[1].clone())
            .or_default()
            .push(i);
    }
    let is_free = |special: &[bool], i: usize| !special[i];
    let big_subs: Vec<&String> = sub_members
        .iter()
        .filter(|(_, m)| m.iter().filter(|&&i| is_free(&special, i)).count() >= 4)
        .map(|(k, _)| k)
        .collect();
    let stride = (big_subs.len() / p.decided_subgroups.max(1)).max(1);
    let mut picked_subs: Vec<String> = big_subs
        .iter()
        .step_by(stride)
        .take(p.decided_subgroups)
        .map(|s| (*s).clone())
        .collect();
    // Sub-groups no outlier, held row, edit or marker touches, decided whole: they come last, so
    // the new-since and missing-record rows the earlier groups carry are already placed.
    let whole: Vec<String> = sub_members
        .iter()
        .filter(|(k, m)| {
            !picked_subs.contains(k) && m.len() >= 2 && m.iter().all(|&i| is_free(&special, i))
        })
        .map(|(k, _)| k.clone())
        .collect();
    let whole_stride = (whole.len() / p.fully_decided_subgroups.max(1)).max(1);
    picked_subs.extend(
        whole
            .into_iter()
            .step_by(whole_stride)
            .take(p.fully_decided_subgroups),
    );
    let big_rules: Vec<&String> = rule_members
        .keys()
        .filter(|k| {
            !picked_subs
                .iter()
                .any(|s| s.starts_with(&format!("sub:{}/", &k[5..])))
        })
        .collect();
    let rule_stride = (big_rules.len() / p.decided_rules.max(1)).max(1);
    let picked_rules: Vec<String> = if p.decided_rules == 0 {
        Vec::new()
    } else {
        big_rules
            .iter()
            .rev()
            .step_by(rule_stride)
            .take(p.decided_rules)
            .map(|s| (*s).clone())
            .collect()
    };
    let states = [
        DecisionState::Confirmed,
        DecisionState::FalsePositive,
        DecisionState::TechDebtLater,
    ];
    let mut nsd_left = p.new_since_decision;
    let mut nsd_for: Vec<(usize, String)> = Vec::new();
    // Gaps: (row index, decision id) of a covered member that holds no record of its own.
    let mut gaps_left = p.decision_gaps;
    let mut gap_for: Vec<(usize, String)> = Vec::new();
    for (n, node) in picked_subs.iter().chain(picked_rules.iter()).enumerate() {
        let members = if node.starts_with("sub:") {
            &sub_members[node]
        } else {
            &rule_members[node]
        };
        let id = format!("dec-node-{n}");
        let state = states[n % states.len()].clone();
        let mut covered = Vec::new();
        let mut left_out = 0;
        for &i in members {
            if special[i] || rows[i].decision.is_some() {
                left_out += 1;
                continue;
            }
            if nsd_left > 0 && node.starts_with("sub:") {
                nsd_left -= 1;
                nsd_for.push((i, id.clone()));
                left_out += 1;
                continue;
            }
            covered.push(i);
        }
        // The decision covers every member in `covered`, but one record is missing: the gap the
        // export gate refuses (the server's `group_decision_gaps`). Sub-groups only, and never the
        // last member, so the decided group keeps a record to show.
        let mut recorded = covered.clone();
        if gaps_left > 0 && node.starts_with("sub:") && recorded.len() > 1 {
            if let Some(i) = recorded.pop() {
                gaps_left -= 1;
                gap_for.push((i, id.clone()));
            }
        }
        for &i in &recorded {
            rows[i].decision = Some(DecisionView {
                id: format!("{id}-{i}"),
                state: state.clone(),
                reason: "Same pattern across the group; decided once.".to_string(),
                scope: Scope::Site,
                inherited_from: Some(node.clone()),
                decided_by: "architect".to_string(),
                decided_at: timestamp(n),
                needed_me: None,
                confirmed_by_client: None,
            });
            rows[i].review_level = level(&format!("{REVIEWED_AS_PATTERN_PREFIX}{node}"));
        }
        node_decisions.push(NodeDecisionRecord {
            node: node.clone(),
            id,
            state,
            reason: "Same pattern across the group; decided once.".to_string(),
            covered: covered.len(),
            left_out,
        });
    }
    for (i, decision_id) in &nsd_for {
        let node = node_decisions.iter().find(|d| d.id == *decision_id);
        rows[*i].new_since_decision = Some(NewSinceDecisionView {
            decision_id: decision_id.clone(),
            node_id: node.map(|d| d.node.clone()).unwrap_or_default(),
            decided_at: timestamp(1),
            covered: node.map_or(0, |d| d.covered),
        });
    }
    // Site decisions on rows nobody has touched.
    let site_states = [
        DecisionState::Confirmed,
        DecisionState::Confirmed,
        DecisionState::Confirmed,
        DecisionState::Ignored,
        DecisionState::FalsePositive,
        DecisionState::TechDebtNow,
        DecisionState::TechDebtLater,
    ];
    let mut made = 0;
    for &k in &order {
        if made >= p.site_decisions {
            break;
        }
        let i = normal[k];
        if special[i]
            || rows[i].decision.is_some()
            || rows[i].new_since_decision.is_some()
            || gap_for.iter().any(|(g, _)| *g == i)
        {
            continue;
        }
        let merge_target = rows[normal[1.min(normal.len() - 1)]].site_key.clone();
        let wants_merge = made % 9 == 8 && rows[i].site_key != merge_target;
        let state = if wants_merge {
            DecisionState::MergedInto(merge_target.clone())
        } else {
            site_states[made % site_states.len()].clone()
        };
        if wants_merge {
            let label = rows
                .iter()
                .find(|r| r.site_key == merge_target)
                .map_or_else(String::new, |r| format!("{}:{}", r.path, r.line));
            rows[i].merged_into = Some(MergedIntoView {
                key: merge_target,
                label,
            });
        }
        let acked = state == DecisionState::Ignored && made % 2 == 0;
        rows[i].decision = Some(DecisionView {
            id: format!("dec-site-{i}"),
            state,
            reason: "Checked the caller; see the ticket.".to_string(),
            scope: Scope::Site,
            inherited_from: None,
            decided_by: "architect".to_string(),
            decided_at: timestamp(made),
            needed_me: None,
            confirmed_by_client: acked.then(|| ClientAck {
                by: "client security lead".to_string(),
                at: timestamp(made + 1),
                note: "Accepted for this release.".to_string(),
            }),
        });
        rows[i].review_level = level(PERSONALLY_REVIEWED);
        made += 1;
    }
    // Decisions on escalated rows (the "needed me" tally).
    for (n, c) in candidates
        .iter()
        .filter(|c| c.escalated)
        .enumerate()
        .filter(|(n, _)| n % 3 == 0)
    {
        if let Some(i) = rows.iter().position(|r| r.site_key == c.key) {
            if rows[i].decision.is_none() && !gap_for.iter().any(|(g, _)| *g == i) {
                rows[i].decision = Some(DecisionView {
                    id: format!("dec-esc-{i}"),
                    state: DecisionState::Confirmed,
                    reason: "Escalated and confirmed.".to_string(),
                    scope: Scope::Site,
                    inherited_from: None,
                    decided_by: "architect".to_string(),
                    decided_at: timestamp(n),
                    needed_me: Some(n % 2 == 0),
                    confirmed_by_client: None,
                });
                rows[i].review_level = level(PERSONALLY_REVIEWED);
            }
        }
    }
    // Engine Check verdicts and the corrections behind them.
    let verdicts = apply_engine_mode(p, &mut rows);

    // Everything else is engine verified, unless held.
    for r in &mut rows {
        if r.review_level.is_none() && r.held.is_none() && r.thrown_out_reason.is_none() {
            r.review_level = level(ENGINE_VERIFIED);
        }
        r.markers = markers_for(r);
    }

    assemble(
        p,
        &layout,
        rows,
        candidates,
        budget,
        (&node_decisions, &gap_for),
        &verdicts,
    )
}

/// The old and new text of a field for a made-up edit.
fn edit_text(row: &SiteRow, field: EditField) -> (String, String) {
    match field {
        EditField::Headline => (
            row.headline.clone(),
            format!("{} (clarified)", row.headline),
        ),
        EditField::Fix => (
            row.fix.clone(),
            format!("{} Then add a regression test.", row.fix),
        ),
        EditField::Detail => (
            row.detail.clone(),
            format!("{} Confirmed by reading the caller.", row.detail),
        ),
        _ => (row.severity.clone(), "high".to_string()),
    }
}

fn apply_edit_value(row: &mut SiteRow, field: EditField, new: &str) {
    match field {
        EditField::Headline => row.headline = new.to_string(),
        EditField::Fix => row.fix = new.to_string(),
        EditField::Detail => row.detail = new.to_string(),
        EditField::Severity => row.stage1.severity = new.to_string(),
        _ => {}
    }
}

/// Counts, tree, shell, engine lane, gate and settings from finished rows.
fn assemble(
    p: &Params,
    layout: &Layout,
    rows: Vec<SiteRow>,
    candidates: Vec<EscalationCandidate>,
    budget: EscalationBudget,
    (node_decisions, gap_for): (&[NodeDecisionRecord], &[(usize, String)]),
    verdicts: &BTreeMap<String, EngineCheckVerdict>,
) -> ReviewDataSet {
    // Node membership at every level.
    let mut node_rows: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        for node in &r.group_path {
            node_rows.entry(node.clone()).or_default().push(i);
        }
    }
    // Labels.
    let mut node_labels: BTreeMap<String, String> = BTreeMap::new();
    let mut cat_seen = std::collections::BTreeSet::new();
    for r in &layout.rules {
        let label = category_parts(r.category).1;
        node_labels.insert(cat_node(r.category), label);
        cat_seen.insert(r.category);
        node_labels.insert(r.node.clone(), format!("{}: {}", r.id, r.label));
        for s in &r.subs {
            node_labels.insert(s.node.clone(), s.label.clone());
        }
    }
    node_labels.insert(
        THROWN_CAT.to_string(),
        "Thrown out by the engine".to_string(),
    );
    node_labels.insert(
        THROWN_RULE.to_string(),
        "THROWN-1: rows the engine discarded".to_string(),
    );
    node_labels.insert(THROWN_SUB.to_string(), "Discarded rows".to_string());

    let counts_of = |members: &[usize]| -> NodeCountsView {
        let mut c = NodeCountsView {
            sites: members.len(),
            ..NodeCountsView::default()
        };
        for &i in members {
            let r = &rows[i];
            match r.stage2.as_ref().map(|s| s.state) {
                Some(EscalationState::Escalated) => c.escalated += 1,
                Some(EscalationState::BelowTheLine) => c.below_the_line += 1,
                _ => {}
            }
            if r.stage2
                .as_ref()
                .is_some_and(|s| s.outlier_reason.is_some())
            {
                c.outliers += 1;
            }
            if r.new_since_decision.is_some() {
                c.new_since_decision += 1;
            }
            if r.markers.contains(&Marker::Stale) {
                c.stale += 1;
            }
            if r.held.is_some() {
                c.held += 1;
            }
            if r.decision.is_some() {
                c.decided += 1;
            }
        }
        c
    };

    // The tree, parents before children.
    let mut nodes = Vec::new();
    let node_decision = |id: &str| -> Option<NodeDecisionSummary> {
        node_decisions
            .iter()
            .find(|d| d.node == id)
            .map(|d| NodeDecisionSummary {
                id: d.id.clone(),
                state: d.state.clone(),
                reason: d.reason.clone(),
                decided_at: timestamp(0),
                covered: d.covered,
                left_out: d.left_out,
            })
    };
    let mut push_node =
        |id: &str, level: TreeLevel, parent: Option<&str>, cause: Option<String>, thrown: bool| {
            let Some(members) = node_rows.get(id) else {
                return;
            };
            let counts = counts_of(members);
            let kind = if counts.escalated + counts.below_the_line > 0 {
                TreeNodeKind::Escalation
            } else {
                TreeNodeKind::Bulk
            };
            let label = node_labels.get(id).cloned().unwrap_or_default();
            nodes.push(TreeNodeView {
                id: id.to_string(),
                level,
                parent: parent.map(str::to_string),
                // The group review writes the summaries; without it there are none.
                summary: p
                    .group_reviewed
                    .then(|| format!("{} sites under {label}.", counts.sites)),
                label,
                kind,
                cause,
                summary_source: SummarySource::Model,
                flags: Vec::new(),
                thrown_out: thrown,
                counts,
                decision: node_decision(id),
                summary_edit: None,
            });
        };
    for c in (0..p.categories).chain(std::iter::once(usize::MAX)) {
        let (cid, thrown) = if c == usize::MAX {
            (THROWN_CAT.to_string(), true)
        } else {
            (cat_node(c), false)
        };
        if !node_rows.contains_key(&cid) {
            continue;
        }
        push_node(&cid, TreeLevel::Category, None, None, thrown);
        if thrown {
            push_node(THROWN_RULE, TreeLevel::Rule, Some(&cid), None, true);
            push_node(
                THROWN_SUB,
                TreeLevel::Subgroup,
                Some(THROWN_RULE),
                Some("The engine kept no usable text.".to_string()),
                true,
            );
            continue;
        }
        for r in layout.rules.iter().filter(|r| r.category == c) {
            push_node(&r.node, TreeLevel::Rule, Some(&cid), None, false);
            for s in &r.subs {
                push_node(
                    &s.node,
                    TreeLevel::Subgroup,
                    Some(&r.node),
                    Some(s.cause.clone()),
                    false,
                );
            }
        }
    }
    let tree = TreeView {
        run_id: RUN_ID.to_string(),
        group_reviewed: p.group_reviewed,
        nodes,
    };

    // The shell.
    let live: Vec<&SiteRow> = rows
        .iter()
        .filter(|r| r.thrown_out_reason.is_none())
        .collect();
    let count_state = |f: &dyn Fn(&DecisionState) -> bool| {
        live.iter()
            .filter(|r| r.decision.as_ref().is_some_and(|d| f(&d.state)))
            .count()
    };
    let confirmed = count_state(&|s| *s == DecisionState::Confirmed);
    let ignored = count_state(&|s| *s == DecisionState::Ignored);
    let false_positive = count_state(&|s| *s == DecisionState::FalsePositive);
    let tech_debt =
        count_state(&|s| matches!(s, DecisionState::TechDebtNow | DecisionState::TechDebtLater));
    let merged = count_state(&|s| matches!(s, DecisionState::MergedInto(_)));
    let decided = confirmed + ignored + false_positive + tech_debt + merged;
    let held_rows: Vec<&SiteRow> = live.iter().copied().filter(|r| r.held.is_some()).collect();

    // The export gate.
    let held_row = |r: &SiteRow| HeldRow {
        key: r.site_key.clone(),
        path: r.path.clone(),
        line: r.line,
        rule_id: r.rule_id.clone(),
        reason: r.held.unwrap_or(HoldReason::EvidenceUnverified),
        detail: r
            .held
            .map(HoldReason::phrase)
            .unwrap_or_default()
            .to_string(),
        node_id: r.group_path.get(2).cloned().unwrap_or_default(),
    };
    // A gap is a covered site with no per-site record. New-since rows are not covered, so they
    // never reach the gate.
    let mut gaps: BTreeMap<String, NodeDecisionGap> = BTreeMap::new();
    for (i, decision_id) in gap_for {
        let record = node_decisions.iter().find(|d| d.id == *decision_id);
        gaps.entry(decision_id.clone())
            .or_insert_with(|| NodeDecisionGap {
                decision_id: decision_id.clone(),
                node_id: record.map(|d| d.node.clone()).unwrap_or_default(),
                missing: Vec::new(),
            })
            .missing
            .push(rows[*i].site_key.clone());
    }
    let gate = ExportGateView {
        held: held_rows
            .iter()
            .copied()
            .filter(|r| r.held != Some(HoldReason::MergedWithoutDecision))
            .map(held_row)
            .collect(),
        merged_without_decision: held_rows
            .iter()
            .copied()
            .filter(|r| r.held == Some(HoldReason::MergedWithoutDecision))
            .map(held_row)
            .collect(),
        node_decisions_missing_members: gaps.into_values().collect(),
    };

    // The Engine Check lane.
    let engine = build_engine(p, &rows, &node_labels, verdicts);

    let unresolved = live.len() - decided;
    let shell = ReviewShellView {
        run_id: RUN_ID.to_string(),
        engine_version: "0.9.4".to_string(),
        counts: ReviewCounts {
            total: live.len(),
            unresolved,
            confirmed,
            ignored,
            false_positive,
            tech_debt,
            merged,
            held: held_rows.len(),
        },
        budget,
        lanes: LaneCounts {
            tree_undecided: unresolved,
            escalations: budget.escalated,
            engine_check: engine.cards.len(),
            engine_check_off: p.sample_size == 0,
            flat_rows: unresolved,
        },
        export_gate: gate.clone(),
        stale: StaleSummary {
            stale_decisions: live.iter().filter(|r| r.stale_decision.is_some()).count(),
            stale_edits: live
                .iter()
                .filter(|r| r.edits.iter().any(|e| e.stale))
                .count(),
            moved: live.iter().filter(|r| r.moved_from.is_some()).count(),
            new_since_decision: live
                .iter()
                .filter(|r| r.new_since_decision.is_some())
                .count(),
        },
        group_reviewed: p.group_reviewed,
    };
    let settings = settings_for(p, &budget);
    ReviewDataSet {
        shell,
        tree,
        rows,
        node_rows,
        candidates,
        node_labels,
        engine,
        gate,
        settings,
    }
}

/// The run every fixture belongs to.
pub const RUN_ID: &str = "run-2026-11-03-a";

/// The settings view for `p` and the budget the run came out with.
pub fn settings_for(p: &Params, budget: &EscalationBudget) -> ReviewSettingsView {
    ReviewSettingsView {
        stage_models: StageModelsView {
            scan: "claude-sonnet-5-5".to_string(),
            calibration: "claude-sonnet-5-5".to_string(),
            group_review: "claude-fable-5-1".to_string(),
            fix: "claude-sonnet-5-5".to_string(),
        },
        escalation: EscalationSettingsView {
            rate_per_kloc: p.rate_per_kloc,
            floor: p.floor,
            counted_lines: p.counted_lines,
            budget: *budget,
            over_budget_reasons: p.over_budget_reasons.clone(),
        },
        engine_check: EngineCheckSettingsView {
            sample_size_per_run: p.sample_size,
            suggested: SAMPLE_SIZE_SUGGESTED,
        },
        limits: SettingsLimits {
            rate_max: ESCALATION_RATE_MAX,
            floor_max: ESCALATION_FLOOR_MAX,
            sample_max: SAMPLE_SIZE_MAX,
        },
    }
}

// ── The Engine Check lane ─────────────────────────────────────────────────────────────────

fn build_engine(
    p: &Params,
    rows: &[SiteRow],
    labels: &BTreeMap<String, String>,
    verdicts: &BTreeMap<String, EngineCheckVerdict>,
) -> EngineCheckLane {
    let picks = pick_sample(p, rows);
    let unread_total = rows
        .iter()
        .filter(|r| r.thrown_out_reason.is_none() && r.stage1.real.is_empty())
        .count();
    let thrown_out_total = rows
        .iter()
        .filter(|r| r.thrown_out_reason.is_some())
        .count();
    let cards: Vec<EngineCheckCard> = picks
        .into_iter()
        .map(|(i, stratum)| {
            let r = &rows[i];
            let st = r.stage2.as_ref();
            let rank = st.and_then(|s| s.global_rank).unwrap_or(0);
            let escalation = match st.map(|s| s.state) {
                Some(EscalationState::Escalated) => {
                    let reason = st
                        .and_then(|s| s.reason)
                        .unwrap_or(EscalationReason::NeedsMoreCode);
                    if st.is_some_and(|s| s.over_budget_reason.is_some()) {
                        EscalationStatus::OverBudget { reason, rank }
                    } else {
                        EscalationStatus::Escalated { reason, rank }
                    }
                }
                Some(EscalationState::BelowTheLine) => EscalationStatus::BelowLine { rank },
                _ => EscalationStatus::NotCandidate,
            };
            let node_path: Vec<BreadcrumbItem> = r
                .group_path
                .iter()
                .map(|id| BreadcrumbItem {
                    id: id.clone(),
                    label: labels.get(id).cloned().unwrap_or_else(|| id.clone()),
                })
                .collect();
            EngineCheckCard {
                site: r.clone(),
                stratum,
                stage1: r.stage1.clone(),
                placement: PlacementView {
                    node_path,
                    subgroup_label: st.map(|s| s.subgroup_label.clone()).unwrap_or_default(),
                    cause: st.map(|s| s.subgroup_cause.clone()).unwrap_or_default(),
                    summary: labels
                        .get(&r.group_path[2])
                        .map(|l| format!("{l}: sites that share one cause.")),
                },
                escalation,
                flagged_by: r.flagged_by.clone(),
                duplicate_link: r.possible_duplicate_of.clone(),
                outliers: st
                    .and_then(|s| {
                        s.outlier_reason.map(|reason| OutlierView {
                            reason,
                            why: s.outlier_why.clone(),
                        })
                    })
                    .into_iter()
                    .collect(),
                review_flags: r.review_flags.clone(),
                verdict: verdicts.get(&r.site_key).cloned(),
            }
        })
        .collect();
    let mut lane = EngineCheckLane {
        run_id: RUN_ID.to_string(),
        setting: p.sample_size,
        off: p.sample_size == 0,
        drawn: cards.len(),
        unread_total,
        thrown_out_total,
        allocation: Vec::new(),
        strata: Vec::new(),
        cards,
    };
    refresh_lane_totals(&mut lane, rows);
    lane
}

/// Recompute the allocation table and the strata rates from the cards' verdicts.
pub fn refresh_lane_totals(lane: &mut EngineCheckLane, rows: &[SiteRow]) {
    let mut by_cat: BTreeMap<String, CategoryAllocation> = BTreeMap::new();
    for card in &lane.cards {
        let cat = card
            .site
            .category
            .clone()
            .unwrap_or_else(|| "Unspecified".to_string());
        let entry = by_cat
            .entry(cat.clone())
            .or_insert_with(|| CategoryAllocation {
                category: cat.clone(),
                drawn: 0,
                unread: rows
                    .iter()
                    .filter(|r| {
                        r.stage1.real.is_empty() && r.category.as_deref() == Some(cat.as_str())
                    })
                    .count(),
                verdicts: 0,
                wrong: 0,
                rate_percent: None,
                margin_percent: None,
                indicative_only: false,
            });
        entry.drawn += 1;
        if let Some(v) = &card.verdict {
            entry.verdicts += 1;
            if !v.right {
                entry.wrong += 1;
            }
        }
    }
    for a in by_cat.values_mut() {
        a.indicative_only = a.drawn < SMALL_CATEGORY_DRAWN;
        (a.rate_percent, a.margin_percent) = rate_and_margin(a.verdicts, a.wrong);
    }
    lane.allocation = by_cat.into_values().collect();
    lane.strata = [SampleStratum::Kept, SampleStratum::ThrownOut]
        .into_iter()
        .map(|stratum| {
            let cards: Vec<&EngineCheckCard> =
                lane.cards.iter().filter(|c| c.stratum == stratum).collect();
            let verdicts = cards.iter().filter(|c| c.verdict.is_some()).count();
            let wrong = cards
                .iter()
                .filter(|c| c.verdict.as_ref().is_some_and(|v| !v.right))
                .count();
            let (rate_percent, margin_percent) = rate_and_margin(verdicts, wrong);
            StratumRate {
                stratum,
                drawn: cards.len(),
                verdicts,
                wrong,
                rate_percent,
                margin_percent,
            }
        })
        .collect();
    lane.drawn = lane.cards.len();
}

fn rate_and_margin(verdicts: usize, wrong: usize) -> (Option<f64>, Option<f64>) {
    if verdicts == 0 {
        return (None, None);
    }
    let n = verdicts as f64;
    let rate = wrong as f64 / n;
    let margin = 1.96 * (rate * (1.0 - rate) / n).sqrt();
    (
        Some((rate * 1000.0).round() / 10.0),
        Some((margin * 1000.0).round() / 10.0),
    )
}

/// The rows the Engine Check draws, in card order: unread kept rows first, then up to two
/// thrown-out rows.
fn pick_sample(p: &Params, rows: &[SiteRow]) -> Vec<(usize, SampleStratum)> {
    if p.sample_size == 0 {
        return Vec::new();
    }
    let unread: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.thrown_out_reason.is_none() && r.stage1.real.is_empty())
        .map(|(i, _)| i)
        .collect();
    let thrown: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.thrown_out_reason.is_some())
        .map(|(i, _)| i)
        .collect();
    let kept_n = p.sample_size.saturating_sub(thrown.len().min(2));
    let stride = (unread.len() / kept_n.max(1)).max(1);
    let mut picks: Vec<(usize, SampleStratum)> = unread
        .iter()
        .step_by(stride)
        .take(kept_n)
        .map(|&i| (i, SampleStratum::Kept))
        .collect();
    picks.extend(
        thrown
            .iter()
            .take(2)
            .map(|&i| (i, SampleStratum::ThrownOut)),
    );
    picks
}

/// Give the cards the verdicts `p.engine_mode` asks for, and the rows the holds and corrections
/// behind them. Only rows nothing else has touched are corrected.
fn apply_engine_mode(p: &Params, rows: &mut [SiteRow]) -> BTreeMap<String, EngineCheckVerdict> {
    let mut verdicts = BTreeMap::new();
    if p.engine_mode == EngineMode::Pending {
        return verdicts;
    }
    let errors = EngineCheckError::ALL;
    let mut wrongs = 0usize;
    for (n, (i, stratum)) in pick_sample(p, rows).into_iter().enumerate() {
        let free = rows[i].held.is_none()
            && rows[i].decision.is_none()
            && rows[i].new_since_decision.is_none()
            && stratum == SampleStratum::Kept;
        let wrong = free
            && match p.engine_mode {
                EngineMode::Pending => false,
                EngineMode::NoWithoutCorrection | EngineMode::NoCorrected => wrongs == 0,
                EngineMode::AllVerdicts => n % 6 == 2,
            };
        if !wrong
            && matches!(
                p.engine_mode,
                EngineMode::NoWithoutCorrection | EngineMode::NoCorrected
            )
        {
            continue;
        }
        let key = rows[i].site_key.clone();
        let mut verdict = EngineCheckVerdict {
            key: key.clone(),
            run_id: RUN_ID.to_string(),
            right: !wrong,
            wrong: None,
            correction: None,
            by: "architect".to_string(),
            at: timestamp(n),
            carried_from: None,
        };
        if wrong {
            wrongs += 1;
            verdict.wrong = Some(errors[wrongs % errors.len()]);
            if p.engine_mode == EngineMode::NoWithoutCorrection {
                rows[i].held = Some(HoldReason::EngineCheckNoWithoutCorrection);
            } else {
                let id = format!("dec-engine-{n}");
                rows[i].decision = Some(DecisionView {
                    id: id.clone(),
                    state: DecisionState::FalsePositive,
                    reason: "Engine Check said No; the row is not a real issue.".to_string(),
                    scope: Scope::Site,
                    inherited_from: None,
                    decided_by: "architect".to_string(),
                    decided_at: timestamp(n),
                    needed_me: None,
                    confirmed_by_client: None,
                });
                rows[i].review_level = level(PERSONALLY_REVIEWED);
                verdict.correction = Some(CorrectionRef::Decision(id));
            }
        }
        verdicts.insert(key, verdict);
    }
    verdicts
}
