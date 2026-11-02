//! Which tree node a rule id belongs under (design 3.4), and which rule family two rule ids are
//! in (design 3A.1 item 2).
//!
//! * A native id maps to `rule:<id>`.
//! * An external tool id (the row has a `preview_tool`) maps to the native rule it enforces
//!   when the mapping table below knows one, else to `rule:<tool>/<id>` under its
//!   `categorize_rule_id` category, else to `cat:other-tools`.
//! * An `AI-` id maps to `rule:ai/<category>` under the row's calibration category, labelled
//!   "Model-named: <category>". An `AI-` row with no valid category goes to
//!   `cat:uncategorised`. No row is ever left out.
//!
//! The tool-to-native pairs are DATA read from the existing tool tables (`lib.rs`'s
//! `external_tool_floor_category` and `finding_security_category`) and the corpus's own
//! `sources[].linter` entries; this module adds no pair of its own.

use std::collections::HashMap;

use crate::onboard::Finding;

/// The category node for a row that nothing could classify.
pub const UNCATEGORISED: &str = "uncategorised";
/// The category node for an external-tool rule with no native counterpart and no category.
pub const OTHER_TOOLS: &str = "other-tools";

/// The native floor rules that carry a security category in `finding_security_category`. A tool
/// rule whose category is shared by exactly one of these is the tool that enforces it.
const FLOOR_RULE_IDS: &[&str] = &[
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-PRIVATE-KEY-1",
    "SEC-NO-VENDOR-TOKEN-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
    "SEC-NO-DISABLED-TLS-1",
    "SEC-NO-UNSAFE-DESERIALIZATION-1",
    "SEC-NO-UNSAFE-HTML-SINK-1",
    "SEC-NO-OPEN-REDIRECT-1",
    "SEC-NO-COMMAND-INJECTION-1",
    "RUBY-AVOID-EVAL-SEND-1",
];

/// Tool rule ids mapped to the native rule they enforce.
#[derive(Debug, Clone, Default)]
pub struct ToolMap {
    /// Normalised tool selector (lower case, no `clippy::` prefix) to native rule id, from the
    /// corpus's `sources[].linter` entries.
    selectors: HashMap<String, String>,
}

impl ToolMap {
    /// A map with only the built-in tool tables (no corpus loaded).
    pub fn builtin() -> Self {
        Self::default()
    }

    /// The built-in tables plus the corpus's own tool selectors.
    pub fn from_corpus(corpus: Option<&camerata_rules::RuleSet>) -> Self {
        let mut selectors: HashMap<String, String> = HashMap::new();
        if let Some(set) = corpus {
            let mut native: Vec<(String, Vec<String>)> =
                crate::native_tool_selectors(set).into_iter().collect();
            native.sort();
            for (native_id, tools) in native {
                for tool in tools {
                    selectors.entry(tool).or_insert_with(|| native_id.clone());
                }
            }
        }
        Self { selectors }
    }

    /// The native rule `tool_rule_id` (an external tool's rule id) enforces, if the tables know
    /// one. Order: the explicit tool tables, then the corpus selectors, then the one native floor
    /// rule that shares the tool rule's security category.
    pub fn native_for(&self, tool_rule_id: &str) -> Option<String> {
        if let Some(native) = crate::external_tool_floor_category(tool_rule_id) {
            return Some(native.to_string());
        }
        let normalised = tool_rule_id.trim().to_ascii_lowercase();
        let normalised = normalised
            .strip_prefix("clippy::")
            .unwrap_or(&normalised)
            .to_string();
        if let Some(native) = self.selectors.get(&normalised) {
            return Some(native.clone());
        }
        let category = crate::finding_security_category(tool_rule_id)?;
        let mut same: Vec<&str> = FLOOR_RULE_IDS
            .iter()
            .copied()
            .filter(|id| crate::finding_security_category(id) == Some(category))
            .collect();
        same.dedup();
        match same.as_slice() {
            [only] => Some((*only).to_string()),
            _ => None,
        }
    }
}

/// Whether the row came from an external tool (a scan-time preview pass), as opposed to the
/// floor, the architectural checkers (which are native) or the model.
pub fn is_external_tool_row(f: &Finding) -> bool {
    f.preview_tool
        .as_deref()
        .is_some_and(|t| t != crate::onboard::ARCH_PREVIEW_TOOL)
}

/// The rule family of a row: the key two rows share when they are the same rule.
///
/// * A native id is its own family (the floor row and the model's row for the same rule share
///   it, R1B-5).
/// * An external tool id is the native rule the mapping assigns it (R1-2), else its own id.
/// * An `AI-` id is never in a native family: its family is the id itself, so only an identical
///   `AI-` id shares it.
///
/// Typos the model made in a native id are canonicalised when its answer is parsed, long before
/// a row reaches here, so equal canonical ids are equal strings.
pub fn family_of(f: &Finding, tools: &ToolMap) -> String {
    if f.rule_id.starts_with("AI-") {
        return format!("ai:{}", f.rule_id);
    }
    if is_external_tool_row(f) {
        if let Some(native) = tools.native_for(&f.rule_id) {
            return native;
        }
    }
    f.rule_id.clone()
}

/// Where a row sits above its sub-group: the category node id, the rule node id and the rule
/// node's display label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulePlacement {
    pub category: String,
    pub rule: String,
    pub rule_label: String,
}

/// The `cat:` node id of a category name.
pub fn category_node_id(category: &str) -> String {
    format!("cat:{category}")
}

/// A readable label for a category node id's name.
pub fn category_label(category: &str) -> String {
    match category {
        UNCATEGORISED => "Uncategorised".to_string(),
        OTHER_TOOLS => "Other tools".to_string(),
        "rls-policy" => "RLS policy".to_string(),
        other => {
            let spaced = other.replace('-', " ");
            let mut chars = spaced.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

/// The closed taxonomy a calibration category must be in to name a node.
fn known_category(category: &str) -> Option<&str> {
    crate::ai_audit::is_known_category(category).then_some(category)
}

/// Place a row: which rule node, and which category node that rule node hangs under, using the
/// row's own category only when the rule id itself says nothing (see [`rule_node_for`]).
pub fn place_row(f: &Finding, tools: &ToolMap) -> RulePlacement {
    rule_node_for(
        &f.rule_id,
        f.preview_tool.as_deref(),
        f.category.as_deref(),
        tools,
    )
}

/// Design 3.4: the rule node for an id.
///
/// `preview_tool` is the row's external tool, if any; `category` is the row's calibration
/// category, used for an `AI-` id and for a native id the rule-id heuristic cannot classify.
pub fn rule_node_for(
    rule_id: &str,
    preview_tool: Option<&str>,
    category: Option<&str>,
    tools: &ToolMap,
) -> RulePlacement {
    let row_category = category.and_then(known_category);
    if rule_id.starts_with("AI-") {
        return match row_category {
            Some(c) => RulePlacement {
                category: category_node_id(c),
                rule: format!("rule:ai/{c}"),
                rule_label: format!("Model-named: {c}"),
            },
            None => RulePlacement {
                category: category_node_id(UNCATEGORISED),
                rule: "rule:ai/uncategorised".to_string(),
                rule_label: "Model-named: uncategorised".to_string(),
            },
        };
    }
    let external = preview_tool.is_some_and(|t| t != crate::onboard::ARCH_PREVIEW_TOOL);
    if external {
        if let Some(native) = tools.native_for(rule_id) {
            return native_placement(&native, row_category);
        }
        let tool = preview_tool.unwrap_or_default();
        let category = crate::ai_audit::categorize_rule_id(rule_id)
            .map_or_else(|| category_node_id(OTHER_TOOLS), |c| category_node_id(&c));
        return RulePlacement {
            category,
            rule: format!("rule:{tool}/{rule_id}"),
            rule_label: format!("{rule_id} ({tool})"),
        };
    }
    native_placement(rule_id, row_category)
}

fn native_placement(rule_id: &str, row_category: Option<&str>) -> RulePlacement {
    let category = crate::ai_audit::categorize_rule_id(rule_id)
        .or_else(|| row_category.map(str::to_string))
        .unwrap_or_else(|| UNCATEGORISED.to_string());
    RulePlacement {
        category: category_node_id(&category),
        rule: format!("rule:{rule_id}"),
        rule_label: rule_id.to_string(),
    }
}
