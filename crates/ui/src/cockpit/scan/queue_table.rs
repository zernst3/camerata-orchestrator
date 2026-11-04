//! The findings queue table: its columns, ordering, bulk bars, the progress strip and the rule hub.

use super::*;

/// The Queue's GROUP-BY column: invisible, and the reason the deviation sentence appears on
/// the group header at all.
///
/// chorale 0.2.3 renders a group header's text from the group-by column's cell value
/// (`build_group_rows` takes `label` straight from `(col.accessor)(row)`), and it exposes NO
/// content-injection hook for that row — `group_header_tr` is private and `group_header_class`
/// is a CSS class, not a slot. So the way to put Camerata's own sentence on a chorale group
/// header, without standing up a parallel grouped renderer beside chorale's, is to make the
/// grouped-on VALUE be the sentence. See [`RULE_GROUP_COLUMN`]'s use in `finding_columns`.
///
/// The column is hidden (`column_visibility`), never sortable and never filterable: it is a
/// grouping key, not data. `build_group_rows` resolves it out of `state.columns` directly, so
/// hiding it does not disturb the grouping.
pub(in crate::cockpit) const RULE_GROUP_COLUMN: ColumnId = ColumnId("rule_group");

/// THE RED FULL-ROW HIGHLIGHT, as a named predicate (design §9.5 — "the red full-row critical
/// highlight stays").
///
/// It reads `severity` and nothing else. The legend §9.5 removed glossed this highlight as
/// "Security findings (deterministic, stop-the-line)", which it never was: a critical
/// AI-ADVISORY finding is highlighted and is neither deterministic nor security, and a
/// deterministic floor hit below `critical` is both and is NOT highlighted. The per-row
/// Severity and Authority badges now carry those two facts separately and correctly.
///
/// Reads the CALIBRATED `FindingView::severity`, never the Severity column's cell value — that
/// one carries a rank prefix (`1 critical`) purely so chorale's lexicographic column sort
/// orders the ladder, and comparing against it here would silently stop matching.
pub(in crate::cockpit) fn finding_row_class(f: &FindingView) -> Option<String> {
    (f.severity == "critical").then(|| "finding-row-critical".to_string())
}

pub(in crate::cockpit) fn finding_columns(
    repos: Vec<String>,
    show_bucket: bool,
) -> Vec<ColumnDef<FindingView>> {
    // chorale 0.2.3's palette has a native orange, so each severity gets a distinct color
    // straight from RenderKind::Badge — no custom cell renderer needed (Critical = red,
    // High = orange, Medium = yellow, Low = gray).
    //
    // The MAP KEYS are `severity_sort_value`s, not bare severities, because that is what the
    // accessor emits (the cell value doubles as the column's sort key — see below). The LABELS
    // are unchanged, so the rendered badge still reads "Critical" / "High" / "Medium" / "Low".
    let sev = BadgeVariantMap::new()
        .with(
            severity_sort_value("critical"),
            BadgeVariant::new("Critical", "red"),
        )
        .with(
            severity_sort_value("high"),
            BadgeVariant::new("High", "orange"),
        )
        .with(
            severity_sort_value("medium"),
            BadgeVariant::new("Medium", "yellow"),
        )
        .with(severity_sort_value("low"), BadgeVariant::new("Low", "gray"));
    let multi_repo = repos.len() > 1;
    let mut cols = vec![
        ColumnDef::new(ColumnId("repo"), "Repo", |f: &FindingView| {
            CellValue::Text(f.repo.clone())
        })
        .sortable()
        .filter(FilterKind::MultiSelect { options: repos })
        .initial_width(180.0),
        // KIND — the most load-bearing new pixel on the row (design §4). Says what the row IS:
        // an objective fault (DEFECT), an inconsistency against the codebase's own observed
        // convention (DEVIATION), or an UNMAPPED row whose producer never assigned a kind.
        //
        // The cell VALUE is the stable wire tag straight from api-types (never re-derived, never
        // parsed from prose), so the multi-select filter is exactly the wire vocabulary. The
        // visible chip is drawn by the row renderer below, which gives `unspecified` the red
        // outline it needs as an ALARM and keeps it visually out of the Defect family.
        //
        // Only the Queue's three tags appear here; the informational kinds never reach this
        // table (they route to their own panels), so listing them as filter options would be a
        // dead affordance.
        ColumnDef::new(ColumnId("kind"), "Kind", |f: &FindingView| {
            CellValue::Text(kind_tag(f).to_string())
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec![
                "defect".to_string(),
                "deviation".to_string(),
                "unspecified".to_string(),
            ],
        })
        .initial_width(130.0),
        // SEVERITY. The cell value is the authored severity PREFIXED BY ITS CALIBRATED LADDER
        // POSITION (`severity_sort_value`), because a sortable chorale column is ordered by
        // exactly the value its accessor returns and `chorale-core` 0.2.3 has no separate
        // sort-key hook. Severity is the one Queue column whose words do not sort into their
        // own meaning — alphabetically `low` sits above `medium` — so without the prefix a
        // header click reads "critical, high, low, medium" in one direction and the same
        // nonsense reversed in the other.
        //
        // This re-derives nothing: the prefix is `severity_rank`, the identical ordering of the
        // identical authored string that `queue_sort_key` applies for the default order, and
        // kind is not an input. The authored word is still what the badge label, the row
        // highlight (`row_class`, which reads `f.severity`), the CSV export (`findings_csv`,
        // hand-rolled off `FindingView`) and the report all read.
        ColumnDef::new(ColumnId("severity"), "Severity", |f: &FindingView| {
            CellValue::Text(severity_sort_value(&f.severity))
        })
        .sortable()
        // Options come from the same function as the accessor, so the filter vocabulary cannot
        // drift from the cell it filters (a stale option list would filter every row out and
        // read as "no findings"). The list is in ladder order, as it was before.
        .filter(FilterKind::MultiSelect {
            options: severity_sort_values(),
        })
        .render_kind(RenderKind::Badge(sev))
        .initial_width(110.0),
        // AUTHORITY, not just provenance: a DETERMINISTIC-FLOOR hit is ENFORCED (regex/logic,
        // repeatable, gateable, stable id); EVERY other finding is ADVISORY (model-inferred,
        // review-only, id/severity may drift run-to-run, never auto-blocks). This is the
        // enforcement-vs-convention split rendered as a column. NOTE: advisory covers BOTH
        // `AI-*` invented ids AND the AI judging code against a corpus rule (e.g. RUST-DIOXUS-11)
        // — the old `AI-` prefix check mislabeled the latter as enforced. Keyed on the floor set.
        // PREVIEW is a THIRD authority tier between enforced and advisory: a scan-time
        // deterministic-tool finding (stable rule-id, no model in the trust path) that is NOT
        // yet wired into the repo's gate. It must read DISTINCTLY from an enforced floor hit
        // ("preview — not enforced until wired") AND from an AI-advisory finding (deterministic,
        // not model-inferred). The CI story still has to wire it for the gate to block on it.
        ColumnDef::new(ColumnId("authority"), "Authority", |f: &FindingView| {
            CellValue::Text(if f.preview {
                "preview".to_string()
            } else if is_enforced_floor(&f.rule_id) {
                "enforced".to_string()
            } else {
                "advisory".to_string()
            })
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec![
                "enforced".to_string(),
                "preview".to_string(),
                "advisory".to_string(),
            ],
        })
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                // chorale 0.2.3 added blue/purple to the palette, so the authorities
                // read as distinct colors (no more gray fallback collision).
                .with("enforced", BadgeVariant::new("Rule · enforced", "green"))
                .with(
                    "preview",
                    BadgeVariant::new("Preview · not enforced until wired", "purple"),
                )
                .with("advisory", BadgeVariant::new("AI · advisory", "blue")),
        ))
        .initial_width(220.0),
        ColumnDef::new(ColumnId("type"), "Finding type", |f: &FindingView| {
            CellValue::Text(f.rule_id.clone())
        })
        .sortable()
        // String lookup, not multi-select: rule ids are many and the architect typically
        // wants "show me everything matching ARCH-" or a specific id, not a checkbox list.
        .filter(FilterKind::Text)
        .initial_width(250.0),
        // CONFIDENCE (design §4, §9.3) — REPLACES the 300px "Needs review" column.
        //
        // > the structured `confidence` field ("high" / "needs-review") as a narrow chip; the
        // > verbose inline reason moves to the detail pane. Recovers ~300px.
        //
        // The column it replaces computed its cell value as
        // `in_test || needs_review || split_needs_review(detail).1.is_some()` — two structured
        // flags OR'd with a REGEX OVER THE MODEL'S OWN ENGLISH — and then drew the matched
        // sentence inline, at 300px, on every row. `confidence_tag` reads structured fields
        // only; see its docs in core for why the prose term was redundant as well as
        // undesirable (the server sets `needs_review` unconditionally two lines above writing
        // the `[needs review: …]` tag, so the regex was reading a shadow of the field beside
        // it) and for why `unrated` is deliberately NOT folded into `high`.
        //
        // THE REASON TEXT IS NOT LOST, it moves: `DetailPaneRow::needs_review` carries it into
        // the docked pane for the focused row, and the finding modal bolds it inline. §9.3's
        // own sentence is "the reason text lives in the detail pane".
        //
        // THE COLUMN ID STAYS `needs_review` on purpose. It is an internal key, never rendered
        // — the header below is what the curator reads, and the order bar resolves its
        // sentence from that header. Renaming it would buy nothing and would break
        // `req_pass2_8_every_meaningful_queue_column_header_is_sortable`, which pins this
        // column as re-cuttable by id; that test asserts a behaviour this change keeps, so
        // editing it to accommodate a cosmetic rename would be loosening a guard for no gain.
        //
        // No custom row-cell renderer any more: the whole point of the removal is that the
        // cell is a CHIP, and chorale's own `RenderKind::Badge` draws chips. The four variants
        // are the exact closed set `confidence_tags()` offers as filter options, which is the
        // same anti-drift discipline the Severity column uses.
        ColumnDef::new(ColumnId("needs_review"), "Confidence", |f: &FindingView| {
            CellValue::Text(confidence_tag(f).to_string())
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: confidence_tags(),
        })
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("test", BadgeVariant::new("Test", "yellow"))
                .with("needs-review", BadgeVariant::new("Needs review", "orange"))
                .with("high", BadgeVariant::new("High", "green"))
                // "Calibration never looked at this row" is a different statement from "it
                // looked and was confident", and the chip says so rather than leaving a blank
                // the curator would read as the latter.
                .with("unrated", BadgeVariant::new("Unrated", "gray")),
        ))
        .initial_width(130.0),
        // The ratchet: enforced (active = new/changed) vs suppressed (baseline debt or
        // an inline waiver). Report shows all; the gate blocks only the enforced ones.
        ColumnDef::new(ColumnId("status"), "Enforcement", |f: &FindingView| {
            CellValue::Text(match f.status.as_str() {
                "suppressed-baseline" => "baseline".to_string(),
                "suppressed-inline" => "waived".to_string(),
                "suppressed-self-reference" => "self-ref".to_string(),
                _ => "enforced".to_string(),
            })
        })
        .sortable()
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("enforced", BadgeVariant::new("Enforced", "red"))
                .with("baseline", BadgeVariant::new("Baseline debt", "gray"))
                .with("waived", BadgeVariant::new("Waived", "yellow"))
                .with("self-ref", BadgeVariant::new("Self-referential", "gray")),
        ))
        .initial_width(150.0),
        // Second grouping level: the FILE (path only). The findings table groups by
        // rule → file, so a rule violated 4× across one file collapses under a
        // "handlers.rs (4)" sub-header instead of 4 loose rows. Path-only (not path:line)
        // so all sites in a file share one group; the line lives in the Line column.
        // Several repos: the value leads with the repo, because `src/index.ts` exists in every
        // service and a bare path would merge them into one group (and one filter match).
        ColumnDef::new(ColumnId("file"), "File", move |f: &FindingView| {
            CellValue::Text(camerata_ui_core::triage::file_group_label(f, multi_repo))
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(260.0),
        ColumnDef::new(ColumnId("loc"), "Line", |f: &FindingView| {
            CellValue::Text(f.line.to_string())
        })
        .sortable()
        .initial_width(80.0),
        ColumnDef::new(ColumnId("snippet"), "Snippet", |f: &FindingView| {
            CellValue::Text(f.snippet.clone())
        })
        .initial_width(380.0),
        // The hidden rule-group key — the deviation sentence's delivery mechanism. See
        // `RULE_GROUP_COLUMN`. Not `.sortable()`, no `.filter(...)`: the curator never
        // interacts with it, and it is excluded from the rendered columns by
        // `finding_table_state`. Its value is `rule_group_label`, which is the bare rule id
        // for a defect and `rule id · N of M sites deviate from "<convention>"` for a
        // deviation, so grouping by it partitions exactly as grouping by the rule id did.
        ColumnDef::new(RULE_GROUP_COLUMN, "Rule group", |f: &FindingView| {
            CellValue::Text(rule_group_label(f))
        }),
    ];
    // The tech-debt bucket flag (resolve later / now). Drawn by a row renderer that reads the
    // live disposition map; the accessor is a placeholder. Only present in the tech-debt view.
    if show_bucket {
        cols.push(
            ColumnDef::new(ColumnId("bucket"), "Bucket", |_f: &FindingView| {
                CellValue::Text(String::new())
            })
            .initial_width(120.0),
        );
    }
    cols
}

/// The Queue's `TableState` exactly as `FindingsTable` mounts it: the real column set, with
/// the grouping key hidden.
///
/// One function rather than an inline construction so the render path and the tests mount the
/// SAME state. A test that forgot the visibility line would be asserting against a table with
/// a stray "Rule group" column the product never shows.
pub(in crate::cockpit) fn finding_table_state(
    rows: Vec<(RowId, FindingView)>,
    repos: Vec<String>,
    show_bucket: bool,
) -> TableState<FindingView> {
    let mut state = TableState::new(rows, finding_columns(repos, show_bucket));
    // Set directly on the state rather than through `handle.set_column_visibility` after
    // mount: the column must never be visible, not even for the first frame, and a transition
    // fired from an effect is one render too late.
    state.column_visibility.insert(RULE_GROUP_COLUMN, false);
    state
}

/// The columns the Queue is grouped by under `preset`, outermost first. The one place a preset's
/// grouping (`OrderPreset::grouping`) is mapped onto chorale's columns; the rule is always the
/// hidden `RULE_GROUP_COLUMN`, whose value is the group header's text (the deviation sentence).
pub(in crate::cockpit) fn group_columns(preset: OrderPreset) -> Vec<ColumnId> {
    match preset.grouping() {
        Grouping::RuleThenFile => vec![RULE_GROUP_COLUMN, ColumnId("file")],
        Grouping::FileThenRule => vec![ColumnId("file"), RULE_GROUP_COLUMN],
        Grouping::Flat => Vec::new(),
    }
}

/// Re-cut the mounted Queue table by `preset`: reorder the rows it holds (the same ids, so
/// nothing about a row's identity changes), drop any manual column sort that would hide the new
/// order, regroup, and open the groups that hold `focused` (the cursor's finding key) so the
/// cursor stays on it. The selection survives: `set_rows` clears it, so it is put back.
pub(in crate::cockpit) fn recut_table(
    handle: chorale_dioxus::UseTableHandle<FindingView>,
    preset: OrderPreset,
    focused: Option<&str>,
) {
    let selected = handle.selected_ids();
    let rows: Vec<(RowId, FindingView)> = handle.signal().peek().rows.as_ref().clone();
    handle.set_rows(order_rows(preset, rows));
    for id in selected {
        handle.set_selection(id, true);
    }
    handle.clear_sort();
    super::table::apply_grouping(handle, &group_columns(preset));
    if let Some(key) = focused {
        // A row the table no longer holds (just decided) has nothing to open; the cursor then
        // clamps to its neighbour as it always does.
        let _ = super::viewer::reach_finding(handle, key);
        // `set_rows` and `clear_sort` put the scroll back at the top; bring the cursor's row
        // back into view.
        if let Some(top) = cursor_scroll_top(&handle.signal().peek(), key) {
            dioxus::document::eval(&scroll_table_js(top));
        }
    }
}

/// How far to scroll the Queue's table so the row of finding `key` sits in the upper third of
/// the viewport, from the table's own row height and viewport (the same arithmetic the code
/// viewer uses for a line: `viewer::scroll_top_for_line`). `None` when the row is not on screen
/// (a collapsed group or a filter hides it).
pub(in crate::cockpit) fn cursor_scroll_top(
    state: &TableState<FindingView>,
    key: &str,
) -> Option<f64> {
    /// What the table's viewport is taken to be until it has measured itself.
    const UNMEASURED_VIEWPORT_PX: f64 = 600.0;
    let view = chorale_core::visible_grouped_view(state);
    let index = view.iter().position(|row| match row {
        chorale_core::GroupedRow::Data(_, f) => finding_key(f) == key,
        _ => false,
    })?;
    let row_h = if state.row_height > 0.0 {
        state.row_height
    } else {
        36.0
    };
    let viewport = if state.viewport_height > 0.0 {
        state.viewport_height
    } else {
        UNMEASURED_VIEWPORT_PX
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(camerata_ui_core::viewer::scroll_top_for_line(
        index + 1,
        row_h.round() as u32,
        viewport,
        view.len(),
    ))
}

/// Script that sets the Queue table's scroll container to `top`. Two animation frames, like the
/// viewer's reference follow: the first lets Dioxus lay out the re-grouped rows, the second runs
/// after the browser has sized the container for them. chorale's own scroll handler then moves
/// its row window to match.
pub(in crate::cockpit) fn scroll_table_js(top: f64) -> String {
    format!(
        "requestAnimationFrame(function(){{requestAnimationFrame(function(){{\
var e=document.querySelector('.queue-split-table [id^=\"chorale-scroll-\"]');\
if(e){{e.scrollTop={top};}}}});}});"
    )
}

/// Human name of a findings column, for the order indicator. Resolved from the live column
/// set (never a second hand-written table), so a renamed or new column can't leave the
/// indicator naming a header that no longer exists; an unknown id degrades to the raw id
/// rather than vanishing.
pub(super) fn finding_column_header(cols: &[ColumnDef<FindingView>], id: ColumnId) -> String {
    cols.iter()
        .find(|c| c.id == id)
        .map_or_else(|| id.as_str().to_string(), |c| c.header.clone())
}

/// The sentence the Queue's order indicator shows for `sort` (chorale's own sort state).
///
/// Empty sort = the order `preset` gave the rows (the curated default, design §5, unless the
/// curator picked another cut), which is also what `clear_sort` restores: chorale's
/// `filtered_sorted_pairs` only reorders when `state.sort` is non-empty, so an empty sort
/// hands back `state.rows` exactly as they were ordered. It is NAMED rather than implied: the
/// default order is a JUDGEMENT (the `unspecified` alarm floated to the top of its severity
/// band, Defect before Deviation, cheapest deviations first), and a curator 300 rows in has to
/// be able to tell at a glance whether he is looking at that judgement or at his own re-cut.
///
/// A manual cut names its column(s) and direction, and says the default is still underneath —
/// which is literally true, because chorale's sort is STABLE: ties in the manual key keep the
/// curated order, so e.g. a cut by Kind still lists the deviations cheapest-ratio-first within
/// the Deviation block. Multi-column (shift+click) cuts are listed in priority order.
pub(in crate::cockpit) fn order_indicator_text(
    preset: OrderPreset,
    sort: &[chorale_core::SortState],
    cols: &[ColumnDef<FindingView>],
) -> String {
    if sort.is_empty() {
        return preset.sentence();
    }
    let cuts: Vec<String> = sort
        .iter()
        .map(|s| {
            let arrow = match s.direction {
                chorale_core::SortDirection::Asc => "\u{2191}",
                chorale_core::SortDirection::Desc => "\u{2193}",
            };
            format!("{} {arrow}", finding_column_header(cols, s.column))
        })
        .collect();
    format!(
        "Your sort: {}; {} underneath",
        cuts.join(", then "),
        preset.underneath()
    )
}

/// The order the Queue is showing, resolved for display: the sentence, plus whether it is the
/// curator's own cut (so the way back has to be offered).
///
/// One struct from one function ([`queue_order`]) rather than two props, so the sentence and
/// the "is this a manual cut" flag cannot disagree — a bar that said "Camerata default order"
/// while offering a reset button, or named a cut while hiding the way back, would be worse
/// than no indicator.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct QueueOrder {
    /// The sentence shown to the curator.
    pub text: String,
    /// True when a manual column cut is active.
    pub manual: bool,
}

/// Resolve chorale's sort state into what the order bar shows.
pub(in crate::cockpit) fn queue_order(
    preset: OrderPreset,
    sort: &[chorale_core::SortState],
    cols: &[ColumnDef<FindingView>],
) -> QueueOrder {
    QueueOrder {
        text: order_indicator_text(preset, sort, cols),
        manual: !sort.is_empty(),
    }
}

/// The Queue's ORDER BAR: which order is on screen, and one click back to the curated default.
///
/// Rendered immediately above the table's header row so the sentence and chorale's own ↑/↓
/// arrow read in one glance. Its own component (rather than inline in `FindingsTable`) because
/// both of its states need to be verifiable under render: SSR cannot click a column header, so
/// a bar buried inside the table component could only ever be checked in its default state.
///
/// `Default order` is shown ONLY while a manual sort is active. On the default it would be a
/// no-op control, and its absence is itself part of the indicator: no button means you are
/// looking at the curated order.
#[component]
pub(in crate::cockpit) fn QueueOrderBar(
    /// The resolved order (see [`queue_order`]).
    order: QueueOrder,
    /// Invoked by `Default order`. The call site clears chorale's whole sort list.
    on_reset: Callback<()>,
    /// How the Queue is cut (work order B.4), shown in the select.
    #[props(default)]
    preset: OrderPreset,
    /// Invoked with the preset the curator picked.
    #[props(default)]
    on_preset: Callback<OrderPreset>,
) -> Element {
    let QueueOrder { text, manual } = order;
    rsx! {
        div { class: "queue-order",
            span { class: "queue-order-eyebrow", "Order" }
            label { class: "queue-order-preset",
                select {
                    class: "queue-order-select",
                    title: "{preset.title()}",
                    "aria-label": "Order the Queue",
                    value: "{preset.key()}",
                    onchange: move |e| {
                        if let Some(p) = OrderPreset::from_key(&e.value()) {
                            on_preset.call(p);
                            // The select took the keyboard when it was used; the pane's keys
                            // (and the cursor keys) work from the pane, so give it back.
                            focus_detail_pane();
                        }
                    },
                    for p in OrderPreset::ALL {
                        option {
                            key: "{p.key()}",
                            value: "{p.key()}",
                            selected: p == preset,
                            title: "{p.title()}",
                            "{p.label()}"
                        }
                    }
                }
            }
            span {
                class: if manual { "queue-order-value manual" } else { "queue-order-value" },
                title: "Click any column header to re-cut the list; click it again to reverse. The curated default is severity, then kind (the unspecified alarm first, then defects, then deviations), then the cheapest deviations first.",
                "{text}"
            }
            if manual {
                button {
                    class: "btn-edit-sm queue-order-reset",
                    title: "Drop every manual column sort and restore Camerata's curated default order.",
                    onclick: move |_| on_reset.call(()),
                    "Default order"
                }
            }
        }
    }
}

/// One deviation rule group as the bulk bar shows it.
///
/// Flat, owned, `PartialEq` data rather than the `DeviationGroup` itself, so the bar is a
/// props-only component that renders (and is therefore testable) without a chorale table, a
/// disposition map or a VirtualDom-minted handle.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct DeviationBulkRow {
    /// The rule id, as the group header shows it.
    pub rule_id: String,
    /// The structured sentence (see `DeviationHeadline::sentence`) — the claim the bulk action
    /// is accepting or rejecting. Repeated here rather than only on the group header because
    /// a bulk button whose subject is somewhere else on screen is exactly the "confident
    /// click, unread claim" failure the convention-commit gate exists to prevent.
    pub sentence: String,
    /// How many rows in THIS view the action would move. Not `deviating_sites` — see
    /// `DeviationGroup::row_count`.
    pub row_count: usize,
}

impl DeviationBulkRow {
    /// Flatten one group into the bar's props. The ONLY place `DeviationBulkRow` is built, so
    /// the bar's sentence is always the group's own `headline.sentence()` — the same builder
    /// the group header's label goes through — and its count is always `row_count()`.
    pub(in crate::cockpit) fn from_group(g: &DeviationGroup) -> Self {
        Self {
            rule_id: g.rule_id.clone(),
            sentence: g.headline.sentence(),
            row_count: g.row_count(),
        }
    }
}

/// RULE-LEVEL BULK ACTIONS for deviation groups (design §2/§4, Pass 2 step 2).
///
/// "The natural unit of a deviation decision is the rule, not the site: if the curator
/// believes the convention is real, all N sites are confirmed together far more often than
/// not." So the deviation decision gets one gesture; the per-site gestures in the toolbar stay
/// for the minority case where one of the N exceptions is itself a false positive.
///
/// ## Why this is a bar and not content inside the group-header `<tr>`
///
/// The design's 2026-10-12 dependency check recorded `render_grouped_row` / `group_header_tr`
/// / `group_header_class` as a chorale 0.2.3 group-header content hook. They are not one:
/// in `chorale-dioxus` 0.2.3 the first two are PRIVATE functions and the third is a `String`
/// CSS class on the `<tr>`. The crate exports only `Table`, `use_table`, `UseTableHandle`,
/// the cell/row renderer types and `RowClass` — there is no slot for host content on a group
/// header, and no `Send + Sync` cell renderer could carry an onclick over a Dioxus `Signal`
/// anyway. The SENTENCE still reaches the real header, through the grouped-on value
/// (`RULE_GROUP_COLUMN`); the BUTTONS cannot, so they sit immediately above the table where
/// every deviation group is visible at once, including on mount when every group is collapsed.
///
/// ## Both directions, and why Confirm comes first
///
/// Step 2 shipped the bulk PLUMBING with only `False positive all N` on it, because none of
/// the four dispositions that existed then honestly meant "yes, this is real, it ships".
/// Step 3 adds `Confirm all N` over the identical group row set, as the owner's ruling
/// required. It is rendered FIRST and styled as the primary action because it is the
/// overwhelmingly more common answer: a deviation group exists precisely because the codebase
/// itself established the convention, so "the convention is real and these are the exceptions"
/// is the default reading, and "the tool mis-parsed all N of these" is the exception.
#[component]
pub(in crate::cockpit) fn DeviationBulkBar(
    /// The deviation groups in the current view, in table order.
    groups: Vec<DeviationBulkRow>,
    /// Invoked with the index into `groups` when `Confirm all N` is clicked. Takes no reason,
    /// because confirming needs none (see `TriageModel::confirm`).
    on_confirm_all: Callback<usize>,
    /// Invoked with the index into `groups` when `False positive all N` is clicked. The call
    /// site owns the reason, the require-reason invariant and the disposition write.
    on_false_positive_all: Callback<usize>,
    /// Open the RULE HUB on this group's rule (design §8). This bar IS the deviation group
    /// header's reachable surface — chorale's real `<tr>` header takes no host content — so
    /// the hub hangs off it rather than off a header that cannot carry a button.
    ///
    /// Defaulted rather than required so the bar's existing render tests, which care about the
    /// bulk decisions and not about navigation, keep compiling and asserting unchanged.
    #[props(default)]
    on_open_hub: Callback<usize>,
    /// Open the STACKED GROUP VIEW on this group (work order B.2): every row as a card with its
    /// surrounding code, to confirm or reject the same fix in many places from one screen. The
    /// decisions made there go through the same paths as the two buttons beside it.
    #[props(default)]
    on_review_stacked: Callback<usize>,
) -> Element {
    // No deviations in this view means no decisions of this shape to make. An empty bar with
    // a heading would be a standing reminder of nothing.
    if groups.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "deviation-bulk",
            span { class: "deviation-bulk-eyebrow",
                "Rule-level decisions"
            }
            for (i, g) in groups.iter().enumerate() {
                div { key: "{g.rule_id}-{i}", class: "deviation-bulk-group",
                    // The rule id is the hub's door here, as it is on every surface that shows
                    // one (design §8). A button rather than the plain span it used to be, so
                    // the gesture is the same one the panels and the detail pane offer.
                    button {
                        class: "rule-hub-link deviation-bulk-rule",
                        title: "Everything this scan knows about {g.rule_id}.",
                        onclick: move |_| on_open_hub.call(i),
                        "{g.rule_id}"
                    }
                    span { class: "deviation-bulk-sentence", "{g.sentence}" }
                    button {
                        class: "btn-edit-sm deviation-bulk-stack",
                        id: "{super::stack::bar_door_id(i)}",
                        title: "Read all {g.row_count} row(s) of this rule stacked, each with the code around it, then confirm or reject the checked ones together.",
                        onclick: move |_| on_review_stacked.call(i),
                        "Review all {g.row_count} stacked"
                    }
                    button {
                        class: "btn-run deviation-bulk-confirm",
                        title: "Confirm every one of this rule's {g.row_count} row(s) in this table in one act: the convention is real and these are genuine exceptions. Identical to selecting them and confirming each. No reason required.",
                        onclick: move |_| on_confirm_all.call(i),
                        "Confirm all {g.row_count}"
                    }
                    button {
                        class: "btn-restart deviation-bulk-fp",
                        // §9.2 removed the always-visible false-positive textarea this button
                        // used to read. It now RAISES the on-demand prompt, which names this
                        // group's own count — a thing a shared textarea beside the table could
                        // never do. The reason is still required and still enforced in the same
                        // place, `TriageModel::dispose_with_undo`.
                        title: "Mark every one of this rule's {g.row_count} row(s) in this table as a false positive, in one act. Identical to selecting them and marking each. Asks for the required reason first.",
                        onclick: move |_| on_false_positive_all.call(i),
                        "False positive all {g.row_count}"
                    }
                }
            }
        }
    }
}

/// The refusal the curator sees when a required reason is blank.
///
/// One function for every refusal path (the per-selection prompt, the bulk-bar prompt, and
/// the detail pane's own), because the require-reason invariant is ONE rule and a curator who
/// meets it worded three ways will reasonably wonder whether it is three rules. It names the
/// count for the same reason [`bulk_reason_heading`] does: a refusal that says "a finding"
/// while twelve rows wait is quietly wrong about the stakes.
pub(in crate::cockpit) fn reason_required_message(state: TriageState, count: usize) -> String {
    let noun = if count == 1 { "finding" } else { "findings" };
    format!(
        "A reason is required to move {count} {noun} to \u{201c}{}\u{201d}. It is recorded in \
         the draft and read by the report export.",
        state.label()
    )
}

/// WHAT a raised toolbar reason prompt will commit when the curator finishes typing.
///
/// The row set is SNAPSHOTTED at the moment the prompt opens, never re-read at commit time.
/// That is the whole reason this is a struct rather than two loose signals: the heading says
/// "3 findings", and the commit has to move those three. Re-reading `handle.selected_ids()`
/// after the curator has typed a sentence would let a stray click between the two change the
/// row set under a heading that already promised a number.
#[derive(Clone, PartialEq)]
pub(in crate::cockpit) struct ToolbarReasonPrompt {
    /// The disposition the reason is for. Only the two that require one ever appear here.
    pub state: TriageState,
    /// The findings the commit will move, carried by value on purpose.
    ///
    /// SELF-CONTAINED: not a `Vec<RowId>` resolved against `id_map`, nor a group INDEX resolved
    /// against `bulk_groups`. Both of those lists are rebuilt every render from the current
    /// visible row set, so between raising the prompt and committing it (as long as the curator
    /// takes to type a sentence) a re-sort, a column filter or a group collapse can change what
    /// the handle points at, after the heading has already promised a number. Carrying the rows
    /// makes "the count the curator read" and "the rows the commit moves" the same list by
    /// construction rather than by timing. Every raiser (the toolbar's selection buttons, the
    /// bar's "False positive all N", the stacked view's checked cards) passes its rows here.
    pub rows: Vec<FindingView>,
}

impl ToolbarReasonPrompt {
    /// How many findings this prompt's commit will move. Drives the heading, so the number the
    /// curator reads is the number the commit uses: the same `Vec`, counted once here and
    /// disposed once there.
    pub(in crate::cockpit) fn count(&self) -> usize {
        self.rows.len()
    }
}

/// THE QUEUE TOOLBAR'S ON-DEMAND REASON PROMPT (design §3, §9.2).
///
/// > The two always-visible reason textareas in the Queue toolbar. Replaced by on-demand
/// > prompts. An empty textarea the curator is not using is noise 95% of the time.
///
/// Props-only, for the reason `ProcessGate` and `DeviationBulkBar` are: SSR cannot click, so
/// the only way to render-test "the prompt the Ignore button raises" is to make the prompt a
/// component that can be handed its open state directly.
///
/// It is NOT the detail pane's prompt and must not be confused with it. The pane's prompt is
/// scoped to the pane's focus and disarms the `C`/`I`/`T`/`F` keys while it is open; this one
/// lives outside the pane entirely, so focusing it fires the pane's own `focusout` and the
/// keys disarm through the existing path rather than through a second rule. The two never
/// render at once in practice (the pane's opens from a pane gesture, this one from a toolbar
/// or bulk-bar gesture) and, critically, neither can commit the other's rows.
///
/// The heading comes from `camerata_ui_core::triage::bulk_reason_heading`, which NAMES THE
/// COUNT. The textareas this replaces never could: a box sitting beside a selection cannot
/// say how many rows the button next to it will move, and a bulk disposition is exactly where
/// "I thought that was three rows" is expensive.
#[component]
pub(in crate::cockpit) fn QueueReasonPrompt(
    /// The raised prompt. The parent mounts this component only when one is open.
    prompt: ToolbarReasonPrompt,
    /// The reason typed so far.
    reason: String,
    on_reason: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
    /// The DOM id of the reason field, so a refused command can put the keyboard back in it. Each
    /// place that mounts the prompt gives its own, because the stacked view's prompt can be up
    /// while the Queue's is.
    #[props(default = QUEUE_REASON_ID.to_string())]
    input_id: String,
) -> Element {
    let heading = bulk_reason_heading(prompt.state, prompt.count());
    rsx! {
        div { class: "queue-reason-prompt",
            label { class: "rule-modal-label", "{heading}" }
            textarea {
                id: "{input_id}",
                class: "addressee-input queue-reason-input",
                rows: "2",
                autofocus: true,
                placeholder: "why (the export reads this)",
                value: "{reason}",
                // Pre-focused for the same reason the pane's prompt is: a prompt that appears
                // on a re-render and takes no focus is a surface the curator has to go find
                // after asking for it.
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_reason.call(e.value()),
                onkeydown: move |e: KeyboardEvent| {
                    // Stop propagation even though this prompt sits outside the detail pane
                    // and the pane's keydown handler is on the pane element: the guarantee
                    // "a letter typed into a reason box is never read as a disposition" should
                    // not depend on where in the tree this happens to be mounted today.
                    e.stop_propagation();
                    match e.key() {
                        Key::Escape => { on_cancel.call(()); }
                        Key::Enter if !e.modifiers().contains(Modifiers::SHIFT) => {
                            e.prevent_default();
                            on_commit.call(());
                        }
                        _ => {}
                    }
                },
            }
            div { class: "queue-prompt-actions",
                button {
                    class: "btn-run queue-prompt-commit",
                    onclick: move |_| on_commit.call(()),
                    "Commit \u{2192}"
                }
                button {
                    class: "btn-edit-sm queue-prompt-cancel",
                    onclick: move |_| on_cancel.call(()),
                    "Cancel"
                }
            }
        }
    }
}

/// THE PROGRESS STRIP (work order B.7): one line of text above the Queue's table saying where the
/// review stands, `Undecided 187 of 612 · Confirmed 310 · Ignored 71 · FP 29 · Tech debt 15`.
///
/// Props-only. The words are `camerata_ui_core::progress::progress_text` and the numbers are the
/// `TriageCounts` the caller took from `TriageModel::counts`; nothing is counted here. It is the
/// same tally for every triage table (it counts the whole Queue), and it re-renders whenever the
/// disposition map changes, so a decision and an undo both move it.
///
/// *The review timer's part (INTERNAL, step 7a).* `eta` is the time-left text ("about 3h 10m left
/// at your pace"), drawn after the counts; the caller passes `None` until 20 decisions carry
/// measured time, and this strip never invents a number. `timer` is the timer's state text and
/// its Pause / Resume button (always visible, a native button so Tab reaches it), and
/// `on_open_stats` the door to the internal stats view. The button binds no key of its own, so
/// the pane keeps all of its keys, and it is marked so that pressing it is not itself counted as
/// the activity that resumes an idle timer.
///
/// *Accessibility.* It is plain text in a labelled group, readable in reading order. It is
/// deliberately NOT an `aria-live` region: the reviewer's way of working is a run of single key
/// presses, and every press changes two of these numbers, so a polite live region would queue a
/// re-reading of the whole line after each one over the top of the reader's own pace. The undo
/// bar already announces each decision; the strip is there to be read when asked for.
#[component]
pub(in crate::cockpit) fn ProgressStrip(
    /// The live tally over the Queue's rows.
    counts: TriageCounts,
    /// THE ESTIMATED TIME LEFT, as text ("about 3h 10m left at your pace"), drawn after the
    /// counts. `None` draws nothing: the estimate needs 20 measured decisions and this strip does
    /// not invent a number.
    #[props(default)]
    eta: Option<String>,
    /// The review timer's state text and button. `None` draws no timer.
    #[props(default)]
    timer: Option<camerata_ui_core::review_timer::TimerStripView>,
    /// The Pause / Resume button was pressed.
    #[props(default)]
    on_toggle_timer: Callback<()>,
    /// Open the internal stats view. `None` draws no door to it.
    #[props(default)]
    on_open_stats: Option<Callback<()>>,
) -> Element {
    let text = progress_text(counts);
    rsx! {
        div {
            class: "progress-strip",
            role: "group",
            "aria-label": "Review progress",
            title: "{FP_MEANING}",
            span { class: "progress-counts", "{text}" }
            if let Some(eta) = eta {
                span { class: "progress-eta", " \u{b7} {eta}" }
            }
            if let Some(timer) = timer {
                span { " \u{b7} " }
                // The one timer control, the same component the viewer and the stack draw.
                super::review::TimerControl { timer, on_toggle: on_toggle_timer }
            }
            if let Some(open) = on_open_stats {
                button {
                    class: "btn-edit-sm progress-stats-btn",
                    "data-tour": camerata_ui_core::walkthrough::target::REVIEW_TIME,
                    title: "Internal review-time stats. Never part of an export.",
                    onclick: move |_| open.call(()),
                    "Review time"
                }
            }
        }
    }
}

/// THE PROCESS GATE: the hint that says what is still outstanding, and the button that commits
/// the ignored and tech-debt buckets.
///
/// Its own props-only component (Pass 2 step 3) for the reason `QueueOrderBar` and
/// `DeviationBulkBar` are: SSR cannot click, and this gate lives inside `ScanResults`, which
/// sits behind `use_resource` network resources and is not SSR-testable at all. Step 1's build
/// note recorded exactly that as a coverage caveat — the gate's live bug (informational rows
/// blocking it forever) was fixed with only the pure derivation under test. As props-only data
/// the gate can now be rendered in any tally state, including the one this step introduces: a
/// Queue where everything is CONFIRMED, nothing is outstanding, and there is still nothing to
/// commit.
///
/// Every judgment it renders comes from [`TriageCounts`], which is unit-tested in core. This
/// component chooses words, not rules.
#[component]
pub(in crate::cockpit) fn ProcessGate(
    /// The live tally over the QUEUE rows (never the raw finding set — see
    /// `TriageModel::counts`).
    counts: TriageCounts,
    /// Whether a Process run is already in flight.
    processing: bool,
    /// Invoked when the button is clicked. The caller owns the commit itself.
    on_process: Callback<()>,
) -> Element {
    let outstanding = counts.outstanding();
    rsx! {
        div { class: "triage-process",
            if outstanding > 0 {
                // The exits are named in full. Before step 3 this sentence offered only
                // "Ignore or save as Tech debt", which was already incomplete (False positive
                // also resolves a row) and would now omit the one the curator reaches for
                // most.
                p { class: "section-hint",
                    "Resolve the {outstanding} remaining Unresolved finding(s) \u{2014} Confirm, \
                     Ignore, save as Tech debt, or mark a false positive \u{2014} before Processing."
                }
            } else if !counts.has_committable_work() {
                // The common end state once Confirm exists: every row decided, and nothing
                // for the server to do. Saying so beats a disabled button with no
                // explanation, which is the dead affordance the standing rule is about.
                p { class: "section-hint",
                    "Nothing to process: no findings were ignored or saved as tech debt. \
                     Confirmed and False positive are recorded in this draft and read by the \
                     report export; neither commits anything to a repository."
                }
            }
            button {
                class: "btn-run",
                disabled: processing || !counts.can_process(),
                onclick: move |_| on_process.call(()),
                if processing { "Processing\u{2026}" } else { "Process ignored + tech-debt buckets" }
            }
        }
    }
}

/// The Kind chip's `(class, label, tooltip)` for a wire kind tag.
///
/// One function, THREE renderers: the Queue's Kind column cell, the docked detail pane's
/// header, and the rule hub strip all call it, so the badge a curator sees in the row, the
/// badge he sees in the pane describing that same row, and the badge in the hub joining them
/// can never say different things. The states are not colours of one thing — `unspecified` is
/// an ALARM about the pipeline and gets a red OUTLINE and its own label, which is why this is
/// hand-rolled rather than a chorale `RenderKind::Badge`.
///
/// The three INFORMATIONAL tags are listed here even though the Queue never shows one, because
/// the hub does: it is reachable from the Conventions, No convention and Not assessed panels,
/// and a convention row falling into the catch-all would be labelled "Unspecified" — an alarm
/// raised about a perfectly healthy row. Listing them also narrows the catch-all to what it
/// should always have been: a tag this UI does not recognise.
pub(in crate::cockpit) fn kind_chip_parts(tag: &str) -> (&'static str, &'static str, &'static str) {
    match tag {
        "convention_observed" => (
            "kind-chip kind-informational",
            "Convention observed",
            "The codebase follows this convention consistently, with zero violations. \
             Informational — there is nothing here to dispose.",
        ),
        "no_convention" => (
            "kind-chip kind-informational",
            "No convention",
            "The codebase is split with no clear majority. One informational row per rule.",
        ),
        "not_assessed" => (
            "kind-chip kind-informational",
            "Not assessed",
            "The rule was armed but could not be evaluated. An honesty row: the scan cannot \
             vouch for it either way.",
        ),
        "defect" => ("kind-chip kind-defect", "Defect", "An objective fault."),
        "deviation" => (
            "kind-chip kind-deviation",
            "Deviation",
            "A consistency violation against the convention this codebase itself \
             predominantly follows. Same severity words as a defect, a different claim.",
        ),
        _ => (
            "kind-chip kind-unspecified",
            "Unspecified",
            "PIPELINE BUG: this row reached the UI with no kind, which means an unmapped \
             emission path. It is NOT a defect — it is a gap to fix upstream.",
        ),
    }
}

// ── THE RULE HUB (design §8) ─────────────────────────────────────────────────────────────

/// WHICH RULE the hub is open on, scoped exactly as `camerata_ui_core::triage::rule_hub` is.
///
/// Carries the repo as well as the rule id because the hub is a per-`(repo, rule)` view: in a
/// multi-repo scan one repo can honestly follow a convention while another deviates from it,
/// and a hub keyed on the rule id alone would merge two different assessments into one strip
/// that states a contradiction as a fact.
#[derive(Clone, PartialEq, Debug)]
pub(in crate::cockpit) struct RuleHubRequest {
    pub repo: String,
    pub rule_id: String,
}

/// THE "THIS RULE IN THIS SCAN" STRIP (design §8) — the one join the workbench builds.
///
/// > Rule -> everything about that rule. This is THE join, and it gets the investment.
///
/// Props-only over `camerata_ui_core::triage::RuleHubView`, which is already the flat owned
/// shape (the same discipline as `DeviationBulkRow` and `DetailPaneRow`): every judgement —
/// which surface the rule is on, what its outcome sentence is, how many sites it touches,
/// whether the invariant is violated — is made in core and unit-tested there. This component
/// chooses words and pixels. SSR can therefore render it in every state, including the two
/// that matter most and are hardest to reach by clicking: a conflicting rule, and a deviation
/// whose denominator the producer could not establish.
///
/// ## The one honesty constraint, rendered
///
/// §8: "conforming sites are not in the data (only counts), so there is no 'show me the 45
/// conforming endpoints' link to offer. Do not fabricate one." The conforming count is
/// therefore rendered as TEXT that says so in the same breath, and the strip offers exactly
/// ONE navigation control: `hub.jump`, the single surface this rule lives on.
#[component]
pub(in crate::cockpit) fn RuleHubStrip(
    hub: RuleHubView,
    /// "Take me to this rule." Invoked with `hub.jump` — never with a surface the caller
    /// chose, so the button and the derivation cannot disagree about where the rule is.
    on_jump: Callback<KindSurface>,
    /// THE ALTERNATIVES CONTEXT (design §9.1), for a multi-option rule the audit evaluated.
    /// `None` for a single-option rule, and for any rule the audit recorded no recommendation
    /// for — which is the majority, so this must render nothing at all rather than an empty
    /// "Evaluated against: (none)" line.
    ///
    /// Read-only here, and deliberately so. The Change select and the staged-override/rescan
    /// flow stay on `RuleAlternativesPanel`; see that component's doc comment for why §9.1
    /// cannot move them. What moves is the FACT and its justification, to the surface every
    /// rule id already opens.
    ///
    /// Defaulted so the strip's existing render tests — which assert, among other things,
    /// that the hub offers exactly ONE control — keep compiling and asserting unchanged.
    #[props(default)]
    alternatives: Option<AlternativeControlsRow>,
) -> Element {
    let (kind_class, kind_label, kind_tip) = kind_chip_parts(hub.kind_tag);
    let jump = hub.jump;
    let jump_label = match jump {
        KindSurface::Queue => "Show in the Queue, filtered to this rule".to_string(),
        other => format!("Show in \u{201c}{}\u{201d}", other.label()),
    };
    rsx! {
        div { class: "rule-hub",
            p { class: "rule-hub-eyebrow", "This rule in this scan" }
            div { class: "rule-hub-head",
                span { class: "rule-hub-id", "{hub.rule_id}" }
                span { class: "{kind_class}", title: "{kind_tip}", "{kind_label}" }
                if !hub.repo.is_empty() {
                    span { class: "rule-modal-tag", "{hub.repo}" }
                }
            }
            p { class: "rule-hub-outcome", "{hub.outcome}" }
            div { class: "rule-hub-meta",
                span { class: "rule-modal-tag", "{hub.row_count} row(s)" }
                // Sites, not rows. A merged finding touches several places and is still ONE
                // decision; conflating the two is the shape C5 shipped.
                span { class: "rule-modal-tag", "{hub.site_count} site(s)" }
                if let Some(label) = hub.convention_label.clone() {
                    span { class: "rule-modal-tag rule-hub-convention", "convention \u{b7} {label}" }
                }
                // The Evaluated-against fact, relocated from `RuleAlternativesPanel`
                // (design §9.1) to the surface that is reachable from every rule id.
                //
                // The raw OPTION ID, and only when the richer line below is absent. When the
                // audit recorded a recommendation for this rule, that line carries the
                // authored LABEL instead — which is what the curator reads everywhere else,
                // and the id is an internal key he has no reason to decode. Rendering both
                // would state one fact twice in two vocabularies.
                if alternatives.is_none() {
                    if let Some(opt) = hub.evaluated_option_id.clone() {
                        span { class: "rule-modal-tag rule-hub-evaluated", "evaluated against \u{b7} {opt}" }
                    }
                }
            }
            // THE ALTERNATIVES CONTEXT (design §9.1): what option this rule was judged
            // against, who chose it, the evidence behind the choice, and WHY — the answer the
            // panel's "Why?" button reveals, rendered directly.
            //
            // Directly, rather than behind a Why? button of its own, for two reasons. The hub
            // is an overlay the curator opened deliberately on ONE rule, so three sentences
            // about that rule are what he came for, not something to hide behind a second
            // click; and a modal raised from inside a modal is a worse surface than the text
            // it would reveal. The panel keeps its button because it lists many rules at once
            // and inline reasoning for all of them would be a wall.
            //
            // It also adds NO control, which is load-bearing: §8's "the hub offers exactly one
            // navigation and no link graph" is pinned by a test that counts the rendered
            // buttons, and this line is read-only precisely so that invariant survives it.
            if let Some(alt) = alternatives.clone() {
                div { class: "rule-hub-alt",
                    p { class: "rule-hub-alt-line",
                        "Evaluated against: "
                        strong { "{alt.evaluated_label}" }
                        span { class: "{alt.badge_class}", "{alt.badge_text}" }
                        if alt.accepted {
                            span { class: "rec-badge rec-badge-accepted", "Accepted" }
                        }
                        if !alt.applicable {
                            span { class: "rec-not-applicable", "Not applicable to this repo" }
                        }
                    }
                    if let Some(ev) = alt.evidence.clone() {
                        span { class: "rec-evidence", "Evidence: {ev}" }
                    }
                    p { class: "rule-hub-alt-why",
                        if alt.reasoning.trim().is_empty() {
                            "No reasoning was recorded for this recommendation."
                        } else {
                            "{alt.reasoning}"
                        }
                    }
                    if alt.hallucinated {
                        span { class: "rec-hallucinated-note",
                            "The model did not return a valid choice for this rule; this is the selected or default option shown instead."
                        }
                    }
                    // Where the DECISION lives, named rather than implied. §9.1 wanted the
                    // Change select here too; it could not come (see `RuleAlternativesPanel`),
                    // and a curator reading "Override, pending rescan" with no way to act on
                    // it needs to be told where the control is rather than left to hunt.
                    p { class: "rule-hub-alt-where",
                        "Change this, or rescan a staged override, in \u{201c}Rule alternatives\u{201d} above the Queue."
                    }
                }
            }
            // COUNTS ONLY, and the sentence says why. Offering a link here would be inventing
            // a data set: the pipeline reports deviating sites and a total, never the
            // conforming set.
            if let Some(c) = hub.conforming {
                p { class: "rule-hub-conforming",
                    "{c.conforming} of {c.assessed} assessed sites conform. The conforming sites are not in the scan data \u{2014} only the counts \u{2014} so there is nothing here to open."
                }
            }
            // The invariant, surfaced rather than re-decided. `conflict_surfaces` is
            // `rules_on_multiple_surfaces`' own verdict for this (repo, rule).
            if !hub.conflict_surfaces.is_empty() {
                {
                    let names: Vec<&'static str> =
                        hub.conflict_surfaces.iter().map(|s| s.label()).collect();
                    rsx! {
                        p { class: "rule-hub-conflict",
                            "Pipeline bug: this rule reached {hub.conflict_surfaces.len()} kind surfaces in this scan ({names.join(\", \")}). A rule emits deviations OR a convention OR one no-convention row OR a not-assessed row \u{2014} never two, so these counts are not trustworthy."
                        }
                    }
                }
            }
            div { class: "rule-hub-actions",
                button {
                    class: "btn-edit-sm rule-hub-jump",
                    onclick: move |_| on_jump.call(jump),
                    "{jump_label}"
                }
            }
        }
    }
}

/// THE RULE HUB MODAL: the strip in an overlay, reachable from every surface that shows a
/// rule id.
///
/// ## Why this is not a strip bolted onto `RuleDetailModal`
///
/// Design §8 says "the existing rule detail modal gains a 'this rule in this scan' strip".
/// The existing modal is [`crate::cockpit::rules::RuleDetailModal`], and it is driven by a
/// `ProposedRuleView` — the corpus row for a rule the scan PROPOSED. A scan's findings include
/// rule ids with no proposed-rule row at all (the deterministic content-rule floor, external
/// tool previews, the model's advisory ids), so hanging the hub off that modal would make the
/// hub unreachable for exactly the rules whose provenance a curator is most likely to question.
/// The hub is therefore its own overlay over core's `rule_hub` derivation, which needs nothing
/// but the scan's own rows; where a `ProposedRuleView` does exist, the full rule detail stays
/// one further click away, the same relationship the detail pane has with the finding modal.
/// The DOM id of the Rule Hub modal, which takes keyboard focus when it opens.
pub(in crate::cockpit) const RULE_HUB_ID: &str = "camerata-rule-hub";

#[component]
pub(in crate::cockpit) fn RuleHubModal(
    hub: RuleHubView,
    on_jump: Callback<KindSurface>,
    on_close: Callback<()>,
    /// See [`RuleHubStrip`]'s `alternatives`. Passed straight through.
    #[props(default)]
    alternatives: Option<AlternativeControlsRow>,
) -> Element {
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            // Focusable, and focused on open, so Esc closes it: while it was the only overlay
            // without a key handler, a click inside it left Esc with nowhere to go. The tab stop
            // also means a click on any text inside keeps focus here rather than dropping to the
            // page.
            div {
                class: "rule-modal rule-hub-modal",
                id: RULE_HUB_ID,
                tabindex: "0",
                onclick: move |e| e.stop_propagation(),
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                onkeydown: move |e: KeyboardEvent| {
                    if e.key() == Key::Escape {
                        e.stop_propagation();
                        on_close.call(());
                    }
                },
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{hub.rule_id}" }
                    button {
                        class: "rule-modal-close",
                        onclick: move |_| on_close.call(()),
                        "\u{2715}"
                    }
                }
                RuleHubStrip { hub, on_jump, alternatives }
            }
        }
    }
}
