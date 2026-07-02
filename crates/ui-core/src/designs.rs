//! Pure presentation logic for the per-project designs list (framework-agnostic).
//!
//! The Design Canvas lists a project's saved designs (each design is a draft work-hierarchy
//! tree). These helpers turn a design summary into the labels + badge classes the Dioxus
//! adapter renders, with no VirtualDom dependency (RUST-HEADLESS-CORE-1). They're unit-tested
//! here so the "Epic · 3 nodes" label and the status→badge-class mapping can't silently drift.

/// One saved design as the BFF reports it (`GET /api/projects/:id/designs`).
///
/// The server sorts newest-first and falls back to "Untitled design" for a missing title,
/// so the adapter renders `title` as-is. `node_type` is the root node's type when known.
#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
pub struct DesignSummary {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub node_type: Option<String>,
    /// "draft" | "published" | "archived".
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub node_count: usize,
    /// Server-provided timestamp string; displayed trimmed via [`short_updated`].
    #[serde(default)]
    pub updated: String,
}

impl DesignSummary {
    /// The one-line meta label under the title, e.g. "Epic · 3 nodes" or just "1 node" when
    /// the root type is unknown. Pluralizes "node" and prefixes the root type when present.
    pub fn meta_label(&self) -> String {
        let nodes = if self.node_count == 1 {
            "1 node".to_string()
        } else {
            format!("{} nodes", self.node_count)
        };
        match self.node_type.as_deref().filter(|t| !t.is_empty()) {
            Some(t) => format!("{t} · {nodes}"),
            None => nodes,
        }
    }

    /// The CSS class for this design's status badge. Mirrors the spine-badge scale:
    /// published → green (`done`), archived → muted (`neutral`), draft/anything else →
    /// neutral-accent (`active`) so an in-progress design reads as live, not muted.
    pub fn status_badge_class(&self) -> &'static str {
        status_badge_class(&self.status)
    }

    /// A short, human label for the status pill text.
    pub fn status_label(&self) -> &'static str {
        status_label(&self.status)
    }
}

/// Map a status string to its badge modifier class (see [`DesignSummary::status_badge_class`]).
pub fn status_badge_class(status: &str) -> &'static str {
    match status {
        "published" => "design-status-badge published",
        "archived" => "design-status-badge archived",
        // "draft" and any unknown/empty value fall back to the draft look.
        _ => "design-status-badge draft",
    }
}

/// A short, human-facing label for a status string (title-cased known values).
pub fn status_label(status: &str) -> &'static str {
    match status {
        "published" => "Published",
        "archived" => "Archived",
        "draft" => "Draft",
        _ => "Draft",
    }
}

/// Trim a server timestamp to a compact display form: keep the date part (before the first
/// space or `T`), so `2026-07-02T14:31:07Z` and `2026-07-02 14:31` both show `2026-07-02`.
/// Empty input yields an empty string (the adapter then renders nothing).
pub fn short_updated(updated: &str) -> String {
    let trimmed = updated.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .split(|c| c == ' ' || c == 'T')
        .next()
        .unwrap_or(trimmed)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(node_type: Option<&str>, node_count: usize, status: &str) -> DesignSummary {
        DesignSummary {
            id: "draft-x".into(),
            title: "Checkout Revamp".into(),
            node_type: node_type.map(String::from),
            status: status.into(),
            node_count,
            updated: "2026-07-02T14:31:07Z".into(),
        }
    }

    #[test]
    fn meta_label_prefixes_type_and_pluralizes_nodes() {
        assert_eq!(summary(Some("Epic"), 3, "draft").meta_label(), "Epic · 3 nodes");
        assert_eq!(summary(Some("Epic"), 1, "draft").meta_label(), "Epic · 1 node");
    }

    #[test]
    fn meta_label_drops_type_when_absent_or_empty() {
        assert_eq!(summary(None, 4, "draft").meta_label(), "4 nodes");
        assert_eq!(summary(Some(""), 2, "draft").meta_label(), "2 nodes");
    }

    #[test]
    fn status_badge_class_maps_the_three_states() {
        assert_eq!(status_badge_class("draft"), "design-status-badge draft");
        assert_eq!(status_badge_class("published"), "design-status-badge published");
        assert_eq!(status_badge_class("archived"), "design-status-badge archived");
        // Unknown / empty falls back to the draft look, never panics.
        assert_eq!(status_badge_class(""), "design-status-badge draft");
        assert_eq!(status_badge_class("weird"), "design-status-badge draft");
    }

    #[test]
    fn status_label_is_title_cased_with_draft_fallback() {
        assert_eq!(status_label("published"), "Published");
        assert_eq!(status_label("archived"), "Archived");
        assert_eq!(status_label("draft"), "Draft");
        assert_eq!(status_label("mystery"), "Draft");
    }

    #[test]
    fn short_updated_keeps_only_the_date_part() {
        assert_eq!(short_updated("2026-07-02T14:31:07Z"), "2026-07-02");
        assert_eq!(short_updated("2026-07-02 14:31"), "2026-07-02");
        assert_eq!(short_updated("2026-07-02"), "2026-07-02");
        assert_eq!(short_updated(""), "");
        assert_eq!(short_updated("   "), "");
    }
}
