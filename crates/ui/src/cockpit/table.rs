//! `CamerataTable`: the single, central wrapper around chorale's headless
//! [`Table`](chorale_dioxus::Table).
//!
//! Every cockpit table used to call chorale's `Table { … }` directly and repeat
//! the same house settings on each call: `theme: Theme::Dark`, `sticky_header:
//! true`, the sort/filter flags, and — for grouped tables — the
//! `set_grouping(...) + collapse_all_groups()` collapse-by-default dance. That
//! drift-prone duplication now lives here, in ONE place. Call sites pass only
//! what genuinely varies per table.
//!
//! This is a THIN wrapper, not a reimplementation: it forwards to chorale's
//! `Table` and keeps chorale's exact API surface for the props it exposes. The
//! only behaviour it ADDS is:
//!
//!   * common defaults — `theme: Theme::Dark`, `sticky_header: true`; and
//!   * collapse-by-default — pass `group_by` and the wrapper groups the table
//!     and collapses every group on mount, using the order that actually works
//!     (group → load all rows → collapse). See [`collapse_page_size`] and the
//!     note on the chorale partial-collapse bug below.
//!
//! ## Why collapse-by-default needs a page-size bump (chorale bug context)
//!
//! chorale's `handle.collapse_all_groups()` discovers the set of group keys to
//! collapse by walking the table's CURRENTLY-VISIBLE grouped view. Under the
//! default `DataRowsOnly` pagination (page size 50), that view only contains the
//! groups whose data rows fall on the current page — so groups whose rows live
//! on page 2+ never get a collapse key and render EXPANDED. The visible symptom
//! is "the first groups are collapsed, the ones further down are open."
//!
//! The Camerata-side workaround (already used by the findings + proposed-rules
//! tables) is to switch the table to `InfiniteScroll` and raise the page size so
//! EVERY group is in the view before collapsing. This wrapper centralizes that
//! exact sequence so every grouped table collapses fully and consistently. The
//! underlying limitation is a chorale bug (tracked for an upstream fix); when it
//! is fixed upstream the page-size bump here becomes a harmless no-op.

use super::{ColumnId, PaginationMode, RowCellRenderers, RowClass, RowId, Table, Theme};
use chorale_dioxus::UseTableHandle;
use dioxus::prelude::*;

/// The page size used when loading every group into view before a
/// collapse-by-default. Large enough to hold any realistic single-screen
/// dataset (the cockpit tables are bounded by a project's rules / a repo's
/// findings), so `collapse_all_groups` sees every group key.
///
/// Pulled out as a `const` + tiny pure fn so the collapse sequence's "load all
/// first" intent is testable without a render.
pub(super) const COLLAPSE_LOAD_PAGE_SIZE: usize = 5000;

/// The page size to set before calling `collapse_all_groups()` so EVERY group is
/// in the view (and therefore gets a collapse key). Returns
/// [`COLLAPSE_LOAD_PAGE_SIZE`]. Kept as a function (not just the const) so the
/// collapse contract — "raise the page size past any real row count, then
/// collapse" — has a single named, unit-tested home.
pub(super) const fn collapse_page_size() -> usize {
    COLLAPSE_LOAD_PAGE_SIZE
}

/// The central Camerata table. Wraps chorale's [`Table`] and applies the house
/// defaults (`Theme::Dark`, `sticky_header`). Pass only what varies per table.
///
/// Defaults match the most common call-site shape, so an omitted prop behaves as
/// it did before migration:
///   * `sort_enabled` defaults to `true` (every cockpit table sorts);
///   * `filter_enabled`, `selection_enabled`, `resize_enabled`,
///     `group_expand_toggle` default to `false`;
///   * `row_cell_renderers` / `row_class` default to empty (no override);
///   * `on_row_click` defaults to `None` (no row-click handler);
///   * `group_by` defaults to empty (ungrouped — no collapse-on-mount).
///
/// When `group_by` is non-empty the wrapper, ONCE on mount, groups the table by
/// those columns and collapses every group (the collapse-by-default behaviour),
/// using the load-all-then-collapse order that works around chorale's paginated
/// `collapse_all_groups`. See this module's docs.
#[component]
pub(super) fn CamerataTable<TRow: Clone + PartialEq + 'static>(
    /// The reactive table handle from `use_table`.
    handle: UseTableHandle<TRow>,
    /// Show sort arrows / sortable headers. Defaults to `true`.
    #[props(default = true)]
    sort_enabled: bool,
    /// Show the per-column filter row. Defaults to `false`.
    #[props(default = false)]
    filter_enabled: bool,
    /// Show the selection checkbox column. Defaults to `false`.
    #[props(default = false)]
    selection_enabled: bool,
    /// Show column-resize handles. Defaults to `false`.
    #[props(default = false)]
    resize_enabled: bool,
    /// Show the grouped expand-all / collapse-all toggle. Defaults to `false`.
    /// No-op unless the table is grouped.
    #[props(default = false)]
    group_expand_toggle: bool,
    /// Per-column full-row renderers (pass-through). Defaults to empty.
    #[props(default)]
    row_cell_renderers: RowCellRenderers<TRow>,
    /// Per-row conditional CSS class (pass-through). Defaults to none.
    #[props(default)]
    row_class: RowClass<TRow>,
    /// Row-click handler (pass-through). Defaults to `None`.
    #[props(default)]
    on_row_click: Option<Callback<RowId>>,
    /// Columns to group by. When non-empty, the table is grouped by these (in
    /// order) AND every group is collapsed on mount. Empty (default) = ungrouped.
    #[props(default)]
    group_by: Vec<ColumnId>,
) -> Element {
    // Collapse-by-default, ONCE on mount, for grouped tables. Order is load-bearing:
    // group → switch to a non-paginated view with a huge page size → collapse. This
    // is the workaround for chorale's `collapse_all_groups` only collapsing the
    // groups currently on the page (see module docs). Ungrouped tables (`group_by`
    // empty) skip this entirely and keep chorale's default pagination.
    {
        let group_by = group_by.clone();
        use_hook(move || {
            if !group_by.is_empty() {
                handle.set_grouping(group_by.clone());
                handle.set_pagination_mode(PaginationMode::InfiniteScroll);
                let _ = handle.set_page_size(collapse_page_size());
                handle.collapse_all_groups();
            }
        });
    }

    rsx! {
        Table {
            handle,
            sort_enabled,
            filter_enabled,
            selection_enabled,
            resize_enabled,
            group_expand_toggle,
            row_cell_renderers,
            row_class,
            on_row_click,
            // House defaults — set in ONE place so every cockpit table matches.
            sticky_header: true,
            theme: Theme::Dark,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{collapse_page_size, COLLAPSE_LOAD_PAGE_SIZE};

    #[test]
    fn collapse_page_size_exceeds_default_pagination() {
        // chorale's default page size is 50; the collapse-load size must be far
        // larger so EVERY group is in the view before `collapse_all_groups` runs.
        // If this regressed below the real max row count, the partial-collapse
        // symptom would return for the rows past the page boundary.
        assert!(collapse_page_size() > 50, "must exceed chorale's default page size");
        assert_eq!(collapse_page_size(), COLLAPSE_LOAD_PAGE_SIZE);
    }

    #[test]
    fn collapse_page_size_is_stable() {
        // The contract is a single named value; guard against an accidental change
        // that silently shrinks the load window.
        assert_eq!(collapse_page_size(), 5000);
    }
}
