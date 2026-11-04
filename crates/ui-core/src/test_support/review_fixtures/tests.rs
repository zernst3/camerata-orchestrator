use std::collections::BTreeSet;

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review::EscalationBudget;
use camerata_api_types::review_view::{
    CorrectionRef, EngineCheckLane, ExportGateView, HoldReason, Marker, QueueView,
    ReviewSettingsView, ReviewShellView, SitesPage, TreeLevel, TreeView, SITES_PAGE_SIZE,
};

use super::dataset::{build, markers_for, EngineMode, Params};
use super::{dataset_30k, dataset_base, fixture, Body, DetailForm, Loaded, FIXTURE_IDS, SEED};

#[test]
fn fixture_30k_is_deterministic() {
    let a = dataset_30k();
    let b = dataset_30k();
    assert_eq!(
        a.to_json_bytes(),
        b.to_json_bytes(),
        "two builds with the same seed differ"
    );
    let live = a
        .rows
        .iter()
        .filter(|r| r.thrown_out_reason.is_none())
        .count();
    assert_eq!(live, 30_000, "the scale set has 30,000 sites");
    assert_eq!(a.shell.counts.total, 30_000);
    assert!(
        (1_000..=3_000).contains(&a.tree.nodes.len()),
        "a few thousand nodes at most, got {}",
        a.tree.nodes.len()
    );
}

/// The scale set has the shape of the server's measurement fixture (WO9c: 30,000 rows over 58
/// categories of 8 rules of 5 sub-groups, 2,828 nodes), generated and not stored.
#[test]
fn fixture_30k_has_the_servers_shape() {
    let ds = dataset_30k();
    let count = |level: TreeLevel| {
        ds.tree
            .nodes
            .iter()
            .filter(|n| n.level == level && !n.thrown_out)
            .count()
    };
    assert_eq!(count(TreeLevel::Category), 58, "about 60 categories");
    assert_eq!(count(TreeLevel::Rule), 462);
    assert_eq!(count(TreeLevel::Subgroup), 2_308);
    let kept = ds.tree.nodes.iter().filter(|n| !n.thrown_out).count();
    assert_eq!(kept, 2_828, "the server's node count");
    // No node is empty: the tree has exactly the nodes that hold sites.
    assert!(ds
        .tree
        .nodes
        .iter()
        .filter(|n| !n.thrown_out)
        .all(|n| n.counts.sites > 0));
    // Ids are unique, so a category past the twelve names the pool has is still its own node.
    let ids: BTreeSet<&String> = ds.tree.nodes.iter().map(|n| &n.id).collect();
    assert_eq!(ids.len(), ds.tree.nodes.len());
    // A few sub-groups need more than one page of sites.
    let big = ds
        .tree
        .nodes
        .iter()
        .filter(|n| n.level == TreeLevel::Subgroup && n.counts.sites > SITES_PAGE_SIZE)
        .count();
    assert!(
        big >= 3,
        "{big} sub-groups of more than {SITES_PAGE_SIZE} sites"
    );
}

/// Severities and effort are the values the server writes (wire values), never display forms.
#[test]
fn severities_and_effort_use_the_servers_wire_values() {
    for ds in [dataset_base(), dataset_30k()] {
        for r in &ds.rows {
            assert!(
                ["critical", "high", "medium", "low"].contains(&r.severity.as_str()),
                "severity {:?}",
                r.severity
            );
            assert!(
                ["critical", "high", "medium", "low"].contains(&r.stage1.severity.as_str()),
                "stage 1 severity {:?}",
                r.stage1.severity
            );
            if let Some(effort) = &r.stage1.effort {
                assert!(
                    ["low", "medium", "high"].contains(&effort.as_str()),
                    "effort {effort:?}"
                );
            }
            for e in &r.edits {
                if e.field == camerata_api_types::edits::EditField::Severity {
                    assert!(
                        ["critical", "high", "medium", "low"].contains(&e.new.as_str()),
                        "{e:?}"
                    );
                }
            }
            if let Some(s) = &r.stage2 {
                for d in &s.disagreements {
                    assert!(d.mine.chars().all(|c| !c.is_uppercase()), "{d:?}");
                }
            }
        }
    }
}

#[test]
fn a_different_seed_gives_different_data() {
    let a = build(&Params::base(SEED));
    let b = build(&Params::base(SEED + 1));
    assert_ne!(a.to_json_bytes(), b.to_json_bytes());
}

#[test]
fn fixtures_cover_every_state() {
    let unique: BTreeSet<&str> = FIXTURE_IDS.iter().copied().collect();
    assert_eq!(unique.len(), FIXTURE_IDS.len(), "no id is listed twice");
    assert_eq!(FIXTURE_IDS.len(), 56);
    for id in FIXTURE_IDS {
        assert!(fixture(id).is_some(), "no fixture for {id}");
    }
    assert!(fixture("tree/not-a-state").is_none());
}

#[test]
fn every_view_round_trips_through_json_into_its_wire_type() {
    // The wire types deny unknown fields, so a fixture that does not parse is not a wire shape.
    for ds in [dataset_base(), build(&Params::base(SEED + 7))] {
        let v = serde_json::to_value(&ds.shell).unwrap();
        assert_eq!(
            serde_json::from_value::<ReviewShellView>(v).unwrap(),
            ds.shell
        );
        let v = serde_json::to_value(&ds.tree).unwrap();
        assert_eq!(serde_json::from_value::<TreeView>(v).unwrap(), ds.tree);
        let page = ds
            .sites_page("cat:authorization", None, SITES_PAGE_SIZE)
            .unwrap();
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!(serde_json::from_value::<SitesPage>(v).unwrap(), page);
        let q = ds.queue(true, 0, 100);
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(serde_json::from_value::<QueueView>(v).unwrap(), q);
        let v = serde_json::to_value(&ds.engine).unwrap();
        assert_eq!(
            serde_json::from_value::<EngineCheckLane>(v).unwrap(),
            ds.engine
        );
        let v = serde_json::to_value(&ds.gate).unwrap();
        assert_eq!(
            serde_json::from_value::<ExportGateView>(v).unwrap(),
            ds.gate
        );
        let v = serde_json::to_value(&ds.settings).unwrap();
        assert_eq!(
            serde_json::from_value::<ReviewSettingsView>(v).unwrap(),
            ds.settings
        );
    }
}

#[test]
fn the_base_set_has_every_marker_and_its_markers_follow_its_fields() {
    let ds = dataset_base();
    for m in Marker::PRECEDENCE {
        assert!(
            ds.rows.iter().any(|r| r.markers.contains(&m)),
            "no row carries {m:?}"
        );
    }
    for r in &ds.rows {
        assert_eq!(r.markers, markers_for(r), "{}", r.site_key);
    }
}

#[test]
fn rows_are_in_the_fixed_order_with_unique_keys() {
    let ds = dataset_base();
    let keys: BTreeSet<&String> = ds.rows.iter().map(|r| &r.site_key).collect();
    assert_eq!(keys.len(), ds.rows.len());
    let rank = |s: &str| match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    };
    for w in ds.rows.windows(2) {
        let a = (
            rank(&w[0].stage1.severity.clone()),
            &w[0].path,
            w[0].line,
            &w[0].site_key,
        );
        let _ = a;
    }
    let sorted = ds.rows.windows(2).all(|w| {
        (rank(&w[0].severity), &w[0].path, w[0].line, &w[0].site_key)
            <= (rank(&w[1].severity), &w[1].path, w[1].line, &w[1].site_key)
    });
    assert!(sorted, "rows are ordered by severity, path, line and key");
}

#[test]
fn node_counts_are_read_from_the_rows_and_add_up_the_tree() {
    let ds = dataset_base();
    for n in &ds.tree.nodes {
        let members = &ds.node_rows[&n.id];
        assert_eq!(n.counts.sites, members.len(), "{}", n.id);
        let decided = members
            .iter()
            .filter(|&&i| ds.rows[i].decision.is_some())
            .count();
        assert_eq!(n.counts.decided, decided, "{}", n.id);
        let held = members
            .iter()
            .filter(|&&i| ds.rows[i].held.is_some())
            .count();
        assert_eq!(n.counts.held, held, "{}", n.id);
        let children: usize = ds
            .tree
            .nodes
            .iter()
            .filter(|c| c.parent.as_deref() == Some(n.id.as_str()))
            .map(|c| c.counts.sites)
            .sum();
        if n.level != TreeLevel::Subgroup {
            assert_eq!(children, n.counts.sites, "children of {} add up", n.id);
        }
    }
    // Parents come before children.
    let mut seen = BTreeSet::new();
    for n in &ds.tree.nodes {
        if let Some(p) = &n.parent {
            assert!(seen.contains(p), "{} before its parent {p}", n.id);
        }
        seen.insert(n.id.clone());
    }
}

#[test]
fn a_node_decision_covers_what_it_says_and_leaves_out_the_rest() {
    let ds = dataset_base();
    let decided: Vec<_> = ds
        .tree
        .nodes
        .iter()
        .filter(|n| n.decision.is_some())
        .collect();
    assert!(decided.len() >= 6, "the base set decides several nodes");
    for n in decided {
        let d = n.decision.as_ref().unwrap();
        let inherited = ds.node_rows[&n.id]
            .iter()
            .filter(|&&i| {
                ds.rows[i]
                    .decision
                    .as_ref()
                    .and_then(|x| x.inherited_from.as_deref())
                    == Some(n.id.as_str())
            })
            .count();
        // The decision covers a record-holding member and a gap member alike (the gap is the
        // covered site with no record of its own).
        let gap_members = ds
            .gate
            .node_decisions_missing_members
            .iter()
            .filter(|g| g.node_id == n.id)
            .map(|g| g.missing.len())
            .sum::<usize>();
        assert_eq!(d.covered, inherited + gap_members, "{}", n.id);
        assert!(d.covered > 0);
    }
}

#[test]
fn the_shell_agrees_with_the_rows_and_the_gate() {
    let ds = dataset_base();
    let c = ds.shell.counts;
    let live = ds
        .rows
        .iter()
        .filter(|r| r.thrown_out_reason.is_none())
        .count();
    assert_eq!(c.total, live);
    assert_eq!(
        c.unresolved + c.confirmed + c.ignored + c.false_positive + c.tech_debt + c.merged,
        c.total
    );
    assert_eq!(
        c.held,
        ds.gate.held.len() + ds.gate.merged_without_decision.len()
    );
    assert!(c.held > 0, "shell/held-count needs held rows");
    assert_eq!(ds.shell.export_gate, ds.gate);
    assert_eq!(ds.shell.lanes.engine_check, ds.engine.cards.len());
    for h in ds.gate.held.iter().chain(&ds.gate.merged_without_decision) {
        assert!(
            ds.row(&h.key).is_some_and(|r| r.held.is_some()),
            "{}",
            h.key
        );
    }
    // A gap is a covered site that holds no record (the server's `group_decision_gaps`); it is
    // neither decided nor new since the decision.
    let gaps = &ds.gate.node_decisions_missing_members;
    assert!(!gaps.is_empty(), "the blocked gate needs a real gap");
    for gap in gaps {
        assert!(!gap.missing.is_empty());
        for key in &gap.missing {
            let row = ds.row(key).unwrap_or_else(|| panic!("{key}"));
            assert!(
                row.decision.is_none(),
                "a gap site holds no decision: {key}"
            );
            assert!(
                row.new_since_decision.is_none(),
                "a gap site is not a new-since row: {key}"
            );
        }
    }
}

#[test]
fn every_held_row_names_the_sub_group_it_sits_in() {
    let ds = dataset_base();
    for h in ds.gate.held.iter().chain(&ds.gate.merged_without_decision) {
        let row = ds.row(&h.key).unwrap_or_else(|| panic!("{}", h.key));
        assert_eq!(
            Some(&h.node_id),
            row.group_path.get(2),
            "{} names its sub-group, as the server's held row does",
            h.key
        );
    }
}

#[test]
fn new_since_rows_alone_never_feed_the_export_gate() {
    let mut p = Params::base(SEED);
    p.decision_gaps = 0;
    let ds = build(&p);
    assert!(
        ds.rows.iter().any(|r| r.new_since_decision.is_some()),
        "the set still has new-since rows"
    );
    assert!(ds.gate.node_decisions_missing_members.is_empty());
    let mut q = Params::base(SEED);
    q.decision_gaps = 2;
    let gaps: usize = build(&q)
        .gate
        .node_decisions_missing_members
        .iter()
        .map(|g| g.missing.len())
        .sum();
    assert_eq!(gaps, 2, "one missing record per requested gap");
}

#[test]
fn the_budget_follows_the_formula_and_the_queue_follows_the_budget() {
    let ds = dataset_base();
    let b = ds.shell.budget;
    let f = EscalationBudget::compute(b.rate_per_kloc, b.floor, b.counted_lines);
    assert_eq!(b.budget, f.budget);
    assert_eq!(
        b.escalated,
        ds.candidates.iter().filter(|c| c.escalated).count()
    );
    assert_eq!(b.over_budget, b.escalated - b.budget);
    assert_eq!(b.candidates, ds.candidates.len());
    let q = ds.queue(false, 0, 100);
    assert_eq!(q.escalated.len(), b.escalated);
    assert!(
        q.below.is_empty() && !q.show_below,
        "below the line is hidden until asked for"
    );
    assert_eq!(q.below_total, b.candidates - b.escalated);
    // Ranks are 1..n with no gaps and the escalated rows come first.
    let ranks: Vec<u32> = ds.candidates.iter().map(|c| c.global_rank).collect();
    assert_eq!(
        ranks,
        (1..=u32::try_from(ranks.len()).unwrap()).collect::<Vec<_>>()
    );
    let first_below = ds.candidates.iter().position(|c| !c.escalated);
    if let Some(p) = first_below {
        assert!(ds.candidates[..p].iter().all(|c| c.escalated));
    }
    // An over-budget row is only ever one of the reasons that may exceed it.
    for c in ds
        .candidates
        .iter()
        .filter(|c| c.over_budget_reason.is_some())
    {
        assert!(ds
            .settings
            .escalation
            .over_budget_reasons
            .contains(&c.reason));
    }
}

#[test]
fn sites_page_pages_every_row_once_in_order_and_never_over_500() {
    let ds = dataset_30k();
    let node = ds
        .tree
        .nodes
        .iter()
        .filter(|n| n.level == TreeLevel::Category)
        .max_by_key(|n| n.counts.sites)
        .unwrap()
        .id
        .clone();
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let page = ds.sites_page(&node, cursor.as_deref(), 10_000).unwrap();
        assert!(
            page.rows.len() <= SITES_PAGE_SIZE,
            "a page holds at most {SITES_PAGE_SIZE}"
        );
        assert_eq!(page.offset, seen.len());
        assert_eq!(page.total, ds.node_rows[&node].len());
        seen.extend(page.rows.iter().map(|r| r.site_key.clone()));
        pages += 1;
        match page.next_cursor {
            Some(c) => {
                // The server's cursor is an opaque hex blob, never a raw key.
                assert!(
                    !c.is_empty() && c.bytes().all(|b| b.is_ascii_hexdigit()),
                    "cursor is hex: {c:?}"
                );
                cursor = Some(c);
            }
            None => break,
        }
    }
    assert!(pages > 1, "a big node takes several pages");
    let expected: Vec<String> = ds.node_rows[&node]
        .iter()
        .map(|&i| ds.rows[i].site_key.clone())
        .collect();
    assert_eq!(seen, expected);
    assert!(ds.sites_page("cat:nope", None, 10).is_none());
}

#[test]
fn the_not_group_reviewed_set_has_no_stage_two_columns() {
    let mut p = Params::base(SEED);
    p.group_reviewed = false;
    let ds = build(&p);
    assert!(!ds.tree.group_reviewed && !ds.shell.group_reviewed);
    assert!(ds.rows.iter().all(|r| r.stage2.is_none()));
    assert!(ds.candidates.is_empty());
    assert!(
        ds.tree.nodes.iter().all(|n| n.summary.is_none()),
        "no node carries a model summary when the group review did not run"
    );
    assert!(
        dataset_base()
            .tree
            .nodes
            .iter()
            .all(|n| n.summary.is_some()),
        "the reviewed set keeps its summaries"
    );
}

#[test]
fn no_fixture_text_has_a_dash() {
    for ds in [dataset_base(), dataset_30k()] {
        let json = String::from_utf8(ds.to_json_bytes()).unwrap();
        assert!(
            !json.contains('\u{2014}'),
            "an em dash is in the fixture text"
        );
        assert!(
            !json.contains('\u{2013}'),
            "an en dash is in the fixture text"
        );
    }
}

// ── The named states ──────────────────────────────────────────────────────────────────────

fn body(id: &str) -> Body {
    fixture(id).unwrap().body
}

fn detail_site(id: &str) -> camerata_api_types::review_view::SiteRow {
    match body(id) {
        Body::Detail(d) => d.site.unwrap_or_else(|| panic!("{id} has no site")),
        _ => panic!("{id} is not a detail state"),
    }
}

#[test]
fn shell_states_cover_empty_loading_error_and_held() {
    assert!(matches!(
        fixture("shell/loading").unwrap().shell,
        Loaded::Loading
    ));
    assert!(matches!(
        fixture("shell/error").unwrap().shell,
        Loaded::Failed(_)
    ));
    let empty = fixture("shell/empty")
        .unwrap()
        .shell
        .ready()
        .cloned()
        .unwrap();
    assert_eq!(empty.counts.total, 0);
    let held = fixture("shell/held-count")
        .unwrap()
        .shell
        .ready()
        .cloned()
        .unwrap();
    assert!(held.counts.held > 0);
}

#[test]
fn tree_states_show_what_their_names_promise() {
    let Body::Tree(empty) = body("tree/empty") else {
        panic!()
    };
    assert!(empty.tree.ready().unwrap().nodes.is_empty());
    assert!(matches!(body("tree/loading"), Body::Tree(t) if t.tree == Loaded::Loading));
    assert!(matches!(body("tree/error"), Body::Tree(t) if matches!(t.tree, Loaded::Failed(_))));

    let Body::Tree(big) = body("tree/30k") else {
        panic!()
    };
    assert_eq!(
        big.tree.ready().unwrap().nodes[0].level,
        TreeLevel::Category
    );
    let page = big.pages.values().next().unwrap();
    assert!(page.rows.len() <= SITES_PAGE_SIZE && page.total >= page.rows.len());

    let Body::Tree(markers) = body("tree/markers") else {
        panic!()
    };
    let shown: std::collections::HashSet<Marker> = markers
        .pages
        .values()
        .flat_map(|p| &p.rows)
        .flat_map(|r| r.markers.clone())
        .collect();
    for m in Marker::PRECEDENCE {
        assert!(shown.contains(&m), "tree/markers does not show {m:?}");
    }
    assert!(
        markers.pages.len() >= 2 && markers.open_nodes.len() > markers.pages.len(),
        "ancestors are open too"
    );

    let Body::Tree(prompt) = body("tree/node-prompt") else {
        panic!()
    };
    let node = prompt.prompt_node.clone().unwrap();
    let n = prompt
        .tree
        .ready()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == node)
        .unwrap();
    assert!(n.decision.is_none());
    assert!(
        n.counts.outliers + n.counts.held > 0,
        "the prompt has something to exclude"
    );

    let Body::Tree(decided) = body("tree/node-decided") else {
        panic!()
    };
    let open_decided = decided
        .tree
        .ready()
        .unwrap()
        .nodes
        .iter()
        .filter(|n| decided.pages.contains_key(&n.id))
        .all(|n| n.decision.is_some());
    assert!(open_decided);

    let nr = fixture("tree/not-group-reviewed").unwrap();
    assert!(!nr.shell.ready().unwrap().group_reviewed);
    assert!(matches!(nr.body, Body::Tree(t) if !t.tree.ready().unwrap().group_reviewed));

    let Body::Tree(narrow) = body("tree/narrow") else {
        panic!()
    };
    assert!(narrow.narrow && narrow.selected_site.is_some());
}

#[test]
fn queue_states_sit_where_their_names_say_against_the_line() {
    let q = |id: &str| match body(id) {
        Body::Queue(q) => q,
        _ => panic!("{id}"),
    };
    assert!(q("queue/empty").queue.ready().unwrap().escalated.is_empty());
    assert!(matches!(q("queue/loading").queue, Loaded::Loading));
    assert!(matches!(q("queue/error").queue, Loaded::Failed(_)));
    let at = q("queue/at-budget").queue.ready().cloned().unwrap();
    assert_eq!(at.budget.over_budget, 0);
    assert_eq!(at.budget.escalated, at.budget.budget);
    let over = q("queue/over-budget").queue.ready().cloned().unwrap();
    assert!(over.budget.over_budget > 0);
    assert!(over
        .escalated
        .iter()
        .any(|r| r.candidate.over_budget_reason.is_some()));
    let lowered = q("queue/line-lowered").queue.ready().cloned().unwrap();
    assert!(lowered.show_below && !lowered.below.is_empty());
    assert!(lowered.below.iter().all(|r| !r.candidate.escalated));
    let toast = q("queue/rate-changed-toast");
    let outcome = toast.rate_toast.unwrap();
    assert!(outcome.applied && outcome.after.budget > outcome.before.budget);
    assert_eq!(toast.queue.ready().unwrap().budget, outcome.after);
    let big = q("queue/30k").queue.ready().cloned().unwrap();
    assert!(big.budget.escalated >= 400 && big.below_total > 100);
    assert!(big.below.is_empty());
}

#[test]
fn engine_states_carry_the_verdicts_their_names_describe() {
    let e = |id: &str| match body(id) {
        Body::Engine(e) => e,
        _ => panic!("{id}"),
    };
    let off = e("engine/lane-off");
    let lane = off.lane.ready().unwrap();
    assert!(lane.off && lane.cards.is_empty() && lane.setting == 0);
    assert!(
        fixture("engine/lane-off")
            .unwrap()
            .shell
            .ready()
            .unwrap()
            .lanes
            .engine_check_off
    );
    assert!(matches!(e("engine/loading").lane, Loaded::Loading));

    let pending = e("engine/card-pending");
    let lane = pending.lane.ready().unwrap();
    assert!(!lane.cards.is_empty() && lane.cards.iter().all(|c| c.verdict.is_none()));
    assert!(lane
        .cards
        .iter()
        .any(|c| c.stratum == camerata_api_types::review_view::SampleStratum::ThrownOut));
    assert!(e("engine/no-list-open").wrong_list_open);

    let held = e("engine/no-pending-correction");
    let lane = held.lane.ready().unwrap();
    let card = &lane.cards[held.selected_card.unwrap()];
    let v = card.verdict.as_ref().unwrap();
    assert!(!v.right && v.wrong.is_some() && v.correction.is_none());
    assert_eq!(
        card.site.held,
        Some(HoldReason::EngineCheckNoWithoutCorrection)
    );
    let shell = fixture("engine/no-pending-correction")
        .unwrap()
        .shell
        .ready()
        .cloned()
        .unwrap();
    assert!(shell
        .export_gate
        .held
        .iter()
        .any(|h| h.key == card.site.site_key));

    let corrected = e("engine/no-corrected");
    let lane = corrected.lane.ready().unwrap();
    let card = &lane.cards[corrected.selected_card.unwrap()];
    let Some(CorrectionRef::Decision(id)) = card.verdict.as_ref().unwrap().correction.clone()
    else {
        panic!()
    };
    assert_eq!(card.site.decision.as_ref().unwrap().id, id);
    assert!(card.site.held.is_none());

    let all = e("engine/all-verdicts");
    let lane = all.lane.ready().unwrap();
    assert!(lane.cards.iter().all(|c| c.verdict.is_some()));
    assert!(lane
        .cards
        .iter()
        .any(|c| c.verdict.as_ref().is_some_and(|v| !v.right)));
    assert!(lane
        .strata
        .iter()
        .any(|s| s.rate_percent.is_some() && s.margin_percent.is_some()));
    for c in lane
        .cards
        .iter()
        .filter(|c| c.verdict.as_ref().is_some_and(|v| !v.right))
    {
        assert!(
            c.verdict.as_ref().unwrap().correction.is_some(),
            "a No in all-verdicts is corrected"
        );
    }

    let small = e("engine/small-category");
    let lane = small.lane.ready().unwrap();
    assert!(lane.allocation.iter().any(|a| a.indicative_only));
    assert!(lane
        .allocation
        .iter()
        .all(|a| a.indicative_only
            == (a.drawn < camerata_api_types::review_view::SMALL_CATEGORY_DRAWN)));
}

#[test]
fn detail_states_each_show_the_thing_they_are_named_for() {
    assert!(matches!(body("detail/empty"), Body::Detail(d) if d.site.is_none()));
    let plain = detail_site("detail/plain");
    assert!(plain.markers.is_empty() && plain.decision.is_none());
    let esc = detail_site("detail/escalated");
    assert!(esc.markers.contains(&Marker::Escalated) && esc.decision.is_none());
    assert!(detail_site("detail/outlier")
        .markers
        .contains(&Marker::Outlier));
    assert!(detail_site("detail/edited-field")
        .edits
        .iter()
        .any(|e| !e.stale));
    assert!(detail_site("detail/stale-edit")
        .edits
        .iter()
        .any(|e| e.stale));
    let sd = detail_site("detail/stale-decision");
    assert!(sd.stale_decision.is_some() && sd.decision.is_none());
    assert!(detail_site("detail/moved").moved_from.is_some());
    assert!(detail_site("detail/new-since-decision")
        .new_since_decision
        .is_some());
    assert_eq!(
        detail_site("detail/held").held,
        Some(HoldReason::EvidenceUnverified)
    );
    let merged = detail_site("detail/merged");
    assert!(matches!(
        merged.decision.as_ref().unwrap().state,
        DecisionState::MergedInto(_)
    ));
    assert!(merged.merged_into.is_some());
    assert!(detail_site("detail/added").added_by_reviewer);
    let needs = detail_site("detail/ignored-needs-ack");
    assert!(needs.decision.unwrap().confirmed_by_client.is_none());
    let acked = detail_site("detail/ignored-acked");
    assert!(acked.decision.unwrap().confirmed_by_client.is_some());
    for (id, form) in [
        (
            "detail/edit-open",
            DetailForm::Edit(camerata_api_types::edits::EditField::Fix),
        ),
        ("detail/merge-picker", DetailForm::MergePicker),
        ("detail/split-picker", DetailForm::SplitPicker),
        ("detail/add-form", DetailForm::AddForm),
    ] {
        let Body::Detail(d) = body(id) else { panic!() };
        assert_eq!(d.form, Some(form), "{id}");
        assert!(d.site.is_some());
    }
    let Body::Detail(m) = body("detail/merge-picker") else {
        panic!()
    };
    assert!(
        !m.siblings.is_empty()
            && m.siblings
                .iter()
                .all(|s| Some(&s.site_key) != m.site.as_ref().map(|x| &x.site_key))
    );
    let Body::Detail(r) = body("detail/add-refused") else {
        panic!()
    };
    assert!(matches!(r.form, Some(DetailForm::AddRefused(m)) if m.contains("already exists")));
}

#[test]
fn settings_and_export_states() {
    let s = |id: &str| match body(id) {
        Body::Settings(s) => s,
        _ => panic!("{id}"),
    };
    let d = s("settings/defaults");
    assert!(d.draft.is_none() && !d.saving && !d.run_in_flight);
    let changed = s("settings/rate-changed");
    assert!(changed.draft.is_some() && changed.preview.is_some());
    let invalid = s("settings/invalid");
    let limits = invalid.view.ready().unwrap().limits;
    assert!(invalid.draft.unwrap().rate_per_kloc > limits.rate_max);
    assert!(s("settings/saving").saving);
    assert!(s("settings/error").save_error.is_some());
    assert!(s("settings/run-in-flight").run_in_flight);

    let Body::Export(clear) = body("export/clear") else {
        panic!()
    };
    assert!(!clear.gate.is_blocked());
    let cleared = fixture("export/clear").unwrap();
    let shell = cleared.shell.ready().unwrap();
    assert!(
        shell.stale.new_since_decision > 0,
        "export/clear clears the gate without zeroing new-since"
    );
    assert_eq!(shell.export_gate, clear.gate);
    assert!(!fixture("export/clear")
        .unwrap()
        .shell
        .ready()
        .unwrap()
        .export_gate
        .is_blocked());
    let Body::Export(blocked) = body("export/blocked") else {
        panic!()
    };
    assert!(blocked.gate.is_blocked() && !blocked.gate.held.is_empty());
    assert!(!blocked.gate.merged_without_decision.is_empty());
}

#[test]
fn engine_modes_other_than_pending_leave_the_rest_of_the_data_alone() {
    let a = build(&Params::base(SEED));
    let mut p = Params::base(SEED);
    p.engine_mode = EngineMode::AllVerdicts;
    let b = build(&p);
    assert_eq!(a.tree.nodes.len(), b.tree.nodes.len());
    assert_eq!(a.candidates, b.candidates);
}

/// Real site keys are `repo NUL path NUL fingerprint NUL ordinal`. A readable fixture key would
/// hide the bug class where a screen puts a key in an id, an href or a `data-` attribute.
#[test]
fn every_fixture_key_has_the_real_shape_with_three_nuls() {
    let check = |what: &str, key: &str| {
        assert_eq!(key.matches('\0').count(), 3, "{what}: {key:?}");
        let parts: Vec<&str> = key.split('\0').collect();
        assert!(
            parts.iter().all(|p| !p.is_empty()),
            "{what}: empty part in {key:?}"
        );
        assert_eq!(parts[2].len(), 16, "{what}: fingerprint is 16 hex digits");
        assert!(parts[2].bytes().all(|b| b.is_ascii_hexdigit()), "{what}");
        assert!(parts[3].bytes().all(|b| b.is_ascii_digit()), "{what}");
    };
    for ds in [dataset_base(), dataset_30k()] {
        for r in &ds.rows {
            check("site_key", &r.site_key);
            for f in &r.flagged_by {
                check("flagged_by", &f.site_key);
            }
            if let Some(m) = &r.moved_from {
                check("moved_from", &m.key);
            }
            if let Some(d) = &r.possible_duplicate_of {
                check("duplicate", &d.key);
            }
            if let Some(m) = &r.merged_into {
                check("merged_into", &m.key);
            }
        }
        for h in ds.gate.held.iter().chain(&ds.gate.merged_without_decision) {
            check("held", &h.key);
        }
        for c in &ds.candidates {
            check("candidate", &c.key);
        }
    }
}

/// A node whose every site is decided is there to be drawn with its check (spec section 2).
#[test]
fn the_base_set_has_a_fully_decided_sub_group_and_the_node_decided_state_opens_it() {
    let ds = dataset_base();
    let whole: Vec<&str> = ds
        .tree
        .nodes
        .iter()
        .filter(|n| n.level == TreeLevel::Subgroup && n.counts.sites > 0)
        .filter(|n| n.counts.decided == n.counts.sites && n.decision.is_some())
        .map(|n| n.id.as_str())
        .collect();
    assert!(!whole.is_empty(), "no sub-group is decided whole");
    let fx = fixture("tree/node-decided").unwrap();
    let Body::Tree(t) = fx.body else {
        panic!("not a tree body");
    };
    assert!(
        whole.iter().any(|w| t.open_nodes.iter().any(|o| o == w)),
        "the state opens a fully decided sub-group"
    );
}

#[test]
fn the_fixture_shell_names_the_inspected_commit_of_every_repo_its_rows_are_in() {
    let ds = dataset_base();
    let repos: std::collections::BTreeSet<&str> = ds.rows.iter().map(|r| r.repo.as_str()).collect();
    assert!(!repos.is_empty());
    for repo in repos {
        let sha = ds
            .shell
            .audited_shas
            .get(repo)
            .expect("a commit for the repo");
        assert_eq!(sha.len(), 40, "{repo}: {sha}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
