//! The queue lane's judgments, over the review fixtures.

use camerata_api_types::review::EscalationReason;
use camerata_api_types::review_view::{EscalationBudgetOutcome, NeededMeSummary, QueueRow};

use super::*;
use crate::test_support::review_fixtures::{dataset_30k, dataset_base};

fn view() -> QueueView {
    dataset_base().queue(true, 0, 100)
}

/// A row with the given reason and global rank, in a band, cut from a fixture row.
fn row_of(reason: EscalationReason, rank: u32, band: Band) -> QueueRow {
    let mut r = view().escalated[0].clone();
    r.candidate.reason = reason;
    // The server lets a reason go over budget only at critical (evidence at any severity); a row
    // cut from the fixture takes that severity so the sentence reads what the server would.
    r.candidate.severity = "critical".to_string();
    r.candidate.global_rank = rank;
    r.candidate.escalated = band != Band::BelowLine;
    r.candidate.over_budget_reason = (band == Band::OverBudget).then_some(reason);
    r.candidate.key = format!("key-{rank}-{}", reason.as_str());
    r
}

#[test]
fn queue_sorts_by_reason_rank_then_global_rank() {
    use EscalationReason::{DataLoss, EvidenceUnverified, SecretExposure, UnauthenticatedReach};
    let rows = [
        row_of(EvidenceUnverified, 3, Band::WithinBudget),
        row_of(SecretExposure, 9, Band::WithinBudget),
        row_of(UnauthenticatedReach, 7, Band::WithinBudget),
        row_of(SecretExposure, 2, Band::WithinBudget),
        row_of(DataLoss, 1, Band::OverBudget),
        row_of(UnauthenticatedReach, 5, Band::BelowLine),
        row_of(UnauthenticatedReach, 4, Band::OverBudget),
    ];
    let mut refs: Vec<&QueueRow> = rows.iter().rev().collect();
    sort_rows(&mut refs);
    let got: Vec<(Band, &str, u32)> = refs
        .iter()
        .map(|r| {
            (
                band_of(r),
                r.candidate.reason.as_str(),
                r.candidate.global_rank,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (Band::WithinBudget, "unauthenticated_reach", 7),
            (Band::WithinBudget, "secret_exposure", 2),
            (Band::WithinBudget, "secret_exposure", 9),
            (Band::WithinBudget, "evidence_unverified", 3),
            (Band::OverBudget, "unauthenticated_reach", 4),
            (Band::OverBudget, "data_loss", 1),
            (Band::BelowLine, "unauthenticated_reach", 5),
        ],
        "above the line first, then over budget, then below; reason rank, then global rank"
    );
}

#[test]
fn the_fixture_queue_is_already_in_the_order_the_sort_gives() {
    let v = view();
    let mut sorted: Vec<&QueueRow> = v.escalated.iter().collect();
    sort_rows(&mut sorted);
    let by_key: Vec<&str> = sorted.iter().map(|r| r.candidate.key.as_str()).collect();
    let sent: Vec<&str> = v
        .escalated
        .iter()
        .map(|r| r.candidate.key.as_str())
        .collect();
    assert_eq!(by_key.len(), sent.len(), "the sort drops and adds nothing");
    let mut a = by_key.clone();
    let mut b = sent.clone();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
}

#[test]
fn lower_the_line_reveals_candidates_without_a_network_call() {
    let v = view();
    assert!(
        v.below_total > 0 && !v.below.is_empty(),
        "the fixture has candidates"
    );
    let mut state = QueueState::default();
    assert!(!state.show_below);
    let hidden = layout(&v, &state);
    let hidden_rows = hidden.row_count();
    assert!(
        !hidden
            .entries
            .iter()
            .any(|e| matches!(e, Entry::BelowHeading(_))),
        "no candidate is drawn until asked"
    );
    assert_eq!(
        hidden.footer.as_deref(),
        Some(
            format!(
                "below the line: {} {} (hidden)",
                thousands(v.below_total),
                if v.below_total == 1 {
                    "candidate"
                } else {
                    "candidates"
                }
            )
            .as_str()
        )
    );

    // Showing them is a flag on the state. The view is borrowed, not changed, and there is no
    // other input to the layout: nothing was read or written to draw the candidates.
    let before = v.clone();
    state.toggle_below();
    let shown = layout(&v, &state);
    assert_eq!(v, before, "the view is untouched");
    assert_eq!(shown.row_count(), hidden_rows + v.below.len());
    let heading = shown
        .entries
        .iter()
        .position(|e| matches!(e, Entry::BelowHeading(_)))
        .expect("a heading above the candidates");
    assert!(
        shown.entries[heading + 1..]
            .iter()
            .all(|e| matches!(e, Entry::Row(r) if band_of(r) == Band::BelowLine)),
        "only candidates follow the heading"
    );
    state.toggle_below();
    assert_eq!(
        layout(&v, &state),
        hidden,
        "and hiding them again restores the page"
    );
}

#[test]
fn the_line_is_drawn_after_the_last_budgeted_row_with_its_sentence() {
    let v = view();
    let l = layout(&v, &QueueState::default());
    let within = v
        .escalated
        .iter()
        .filter(|r| band_of(r) == Band::WithinBudget)
        .count();
    let line_at = l
        .entries
        .iter()
        .position(|e| matches!(e, Entry::Line(_)))
        .expect("the line is drawn");
    // `within` rows precede the line.
    assert_eq!(
        l.entries[..line_at]
            .iter()
            .filter(|e| matches!(e, Entry::Row(_)))
            .count(),
        within
    );
    let Entry::Line(text) = &l.entries[line_at] else {
        unreachable!()
    };
    assert_eq!(
        text,
        &format!(
            "the line: {} of a budget of {}",
            thousands(within),
            thousands(v.budget.budget)
        )
    );
    // Over-budget rows sit between the line and the candidates, under their own heading.
    let over = v.escalated.len() - within;
    assert!(over > 0, "the base fixture is over budget");
    let Entry::OverBand(band) = &l.entries[line_at + 1] else {
        panic!(
            "the over-budget band follows the line: {:?}",
            l.entries[line_at + 1]
        );
    };
    assert!(
        band.starts_with(&format!("{over} over budget: all ")),
        "{band}"
    );
    assert!(band.contains("at critical"), "{band}");
}

#[test]
fn an_at_budget_queue_says_so_and_has_no_over_band() {
    let mut v = view();
    v.escalated.retain(|r| band_of(r) == Band::WithinBudget);
    v.budget.budget = v.escalated.len();
    let l = layout(&v, &QueueState::default());
    assert!(!l.entries.iter().any(|e| matches!(e, Entry::OverBand(_))));
    let Some(Entry::Line(text)) = l.entries.iter().find(|e| matches!(e, Entry::Line(_))) else {
        panic!("the line is drawn");
    };
    let n = v.escalated.len();
    assert_eq!(text, &format!("the line: {n} of a budget of {n}"));
    assert!(
        !counts_line(&v).contains("over budget"),
        "{}",
        counts_line(&v)
    );
}

#[test]
fn the_table_never_draws_more_rows_than_its_page_and_the_line_waits_for_its_rows() {
    let big = dataset_30k().queue(false, 0, 100);
    let mut state = QueueState::default();
    let l = layout(&big, &state);
    assert!(l.row_count() <= ROWS_PER_PAGE);
    if big.escalated.len() > ROWS_PER_PAGE {
        assert_eq!(l.hidden_escalated, big.escalated.len() - l.row_count());
        let within = big
            .escalated
            .iter()
            .filter(|r| band_of(r) == Band::WithinBudget)
            .count();
        if within > ROWS_PER_PAGE {
            assert!(
                !l.entries.iter().any(|e| matches!(e, Entry::Line(_))),
                "the line is below the fold until its rows are drawn"
            );
        }
    }
    // Showing more draws 100 more, never all at once past what was asked.
    let mut synthetic = view();
    let template = synthetic.escalated[0].clone();
    for i in 0..450u32 {
        let mut r = template.clone();
        r.candidate.key = format!("k{i}");
        r.candidate.global_rank = 1000 + i;
        r.candidate.over_budget_reason = None;
        synthetic.escalated.push(r);
    }
    let total = synthetic.escalated.len();
    state.shown = ROWS_PER_PAGE;
    assert_eq!(layout(&synthetic, &state).row_count(), ROWS_PER_PAGE);
    state.show_more();
    assert_eq!(layout(&synthetic, &state).row_count(), 2 * ROWS_PER_PAGE);
    state.shown = total + 1;
    let all = layout(&synthetic, &state);
    assert_eq!(all.row_count(), total);
    assert_eq!(all.hidden_escalated, 0);
}

#[test]
fn the_sentences_above_the_table_read_like_the_spec() {
    let v = view();
    assert_eq!(
        budget_line(&v),
        format!(
            "Budget {} of {} lines at {:?} per 1,000 (floor {})",
            thousands(v.budget.budget),
            thousands(v.budget.counted_lines),
            v.budget.rate_per_kloc,
            thousands(v.budget.floor)
        )
    );
    assert!(budget_line(&v).contains("per 1,000 (floor 20)"));
    let counts = counts_line(&v);
    assert!(
        counts.starts_with(&format!("Escalated {}", thousands(v.escalated.len()))),
        "{counts}"
    );
    assert!(counts.contains(" over budget (all "), "{counts}");
    assert!(counts.ends_with("candidates below the line"), "{counts}");
    let n = NeededMeSummary {
        decided: 140,
        needed_me: 120,
        percent: 86,
    };
    assert_eq!(needed_me_line(&n), "Needed me: 120 of 140 decided (86%)");
    assert_eq!(
        needed_me_line(&NeededMeSummary::default()),
        "Needed me: nothing decided yet"
    );
}

#[test]
fn the_over_budget_sentence_names_critical_reach_and_unverified_evidence_apart() {
    use EscalationReason::{EvidenceUnverified, SecretExposure, UnauthenticatedReach};
    let a = row_of(UnauthenticatedReach, 1, Band::OverBudget);
    let b = row_of(SecretExposure, 2, Band::OverBudget);
    let c = row_of(EvidenceUnverified, 3, Band::OverBudget);
    assert_eq!(
        over_budget_reasons_text(&[&b, &a]),
        "all unauthenticated reach or secret exposure at critical"
    );
    assert_eq!(
        over_budget_reasons_text(&[&c]),
        "all unverified evidence at any severity"
    );
    assert_eq!(
        over_budget_reasons_text(&[&a, &c]),
        "all unauthenticated reach at critical, and unverified evidence at any severity"
    );
}

#[test]
fn the_over_budget_sentence_names_the_severities_the_rows_have() {
    use EscalationReason::{EvidenceUnverified, SecretExposure, UnauthenticatedReach};
    let mut low = row_of(UnauthenticatedReach, 1, Band::OverBudget);
    low.candidate.severity = "low".to_string();
    let mut crit = row_of(SecretExposure, 2, Band::OverBudget);
    crit.candidate.severity = "critical".to_string();
    let mut also_crit = row_of(UnauthenticatedReach, 3, Band::OverBudget);
    also_crit.candidate.severity = "critical".to_string();
    // The sentence reads the rows, not the reason: a Low row is never "at critical".
    let sentence = over_budget_reasons_text(&[&low]);
    assert!(!sentence.contains("critical"), "{sentence}");
    assert_eq!(sentence, "all unauthenticated reach at low");
    // Different reasons at different severities are said apart.
    assert_eq!(
        over_budget_reasons_text(&[&low, &crit]),
        "all unauthenticated reach at low, and secret exposure at critical"
    );
    // A reason whose rows differ in severity names each severity present, highest first.
    assert_eq!(
        over_budget_reasons_text(&[&low, &also_crit]),
        "all unauthenticated reach at critical or low"
    );
    // Evidence stays "at any severity", whatever the rows' own severity.
    let mut ev = row_of(EvidenceUnverified, 4, Band::OverBudget);
    ev.candidate.severity = "medium".to_string();
    assert_eq!(
        over_budget_reasons_text(&[&ev]),
        "all unverified evidence at any severity"
    );
}

#[test]
fn the_rate_changed_toast_says_the_budget_the_move_and_that_no_model_was_called() {
    let v = view();
    let mut after = v.budget;
    after.budget = 450;
    let outcome = EscalationBudgetOutcome {
        applied: true,
        before: v.budget,
        after,
        moved_above: 103,
        moved_below: 0,
    };
    assert_eq!(
        rate_changed_text(&outcome),
        "Budget is now 450. 103 candidates moved above the line. No model call was made."
    );
}

#[test]
fn a_candidate_below_the_line_has_no_needed_me_box_and_a_row_needs_deciding_first() {
    let v = view();
    let below = &v.below[0];
    assert!(!has_needed_me_box(below));
    assert!(!needed_me_enabled(below));
    assert_eq!(needed_me_write(below), None);

    let undecided = v.escalated.iter().find(|r| r.site.decision.is_none());
    if let Some(row) = undecided {
        assert!(has_needed_me_box(row));
        assert!(!needed_me_enabled(row), "{NEEDED_ME_HINT}");
        assert_eq!(needed_me_write(row), None);
    }
    let mut decided = v.escalated[0].clone();
    let d = decided.site.decision.get_or_insert_with(|| {
        let mut any = dataset_base()
            .rows
            .iter()
            .find_map(|r| r.decision.clone())
            .expect("a decided row");
        any.needed_me = None;
        any
    });
    d.needed_me = None;
    let id = d.id.clone();
    assert!(needed_me_enabled(&decided));
    assert!(!needed_me_checked(&decided));
    assert_eq!(needed_me_write(&decided), Some((id.clone(), true)));
    decided.site.decision.as_mut().unwrap().needed_me = Some(true);
    assert!(needed_me_checked(&decided));
    assert_eq!(
        needed_me_write(&decided),
        Some((id, false)),
        "a ticked box unticks"
    );
}

#[test]
fn the_cells_read_the_reason_the_node_and_the_door() {
    let v = view();
    let over = v
        .escalated
        .iter()
        .find(|r| band_of(r) == Band::OverBudget)
        .unwrap();
    assert!(reason_cell(over).ends_with(" (over)"));
    let within = v
        .escalated
        .iter()
        .find(|r| band_of(r) == Band::WithinBudget)
        .unwrap();
    assert!(!reason_cell(within).contains("(over)"));
    assert!(reason_cell(within).chars().next().unwrap().is_uppercase());
    assert_eq!(
        Some(node_cell(within).as_str()),
        within.node_path.last().map(|c| c.label.as_str()),
        "the cell is the sub-group; the whole path is its title"
    );
    assert!(node_title(within).contains(" > "), "{}", node_title(within));
    assert_eq!(
        node_id_of(within),
        within.node_path.last().map(|c| c.id.as_str())
    );
    match site_door(within) {
        Door::Site { key, node_id, .. } => {
            assert_eq!(key, within.candidate.key);
            assert_eq!(
                Some(node_id.as_str()),
                within.site.group_path.get(2).map(String::as_str)
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(node_door(within), Some(Door::Node { .. })));
}

#[test]
fn queue_keys_fire_only_with_the_lane_focused_and_no_modifier() {
    let none = KeyMods::none();
    for focus in [PaneFocus::Elsewhere, PaneFocus::ReasonPrompt] {
        for key in ["ArrowDown", "Enter", "c", "i", "f", "t", "n", "g", "s"] {
            assert_eq!(
                queue_command(focus, key, none, true),
                None,
                "{key} under {focus:?}"
            );
        }
    }
    let ctrl = KeyMods { ctrl: true, ..none };
    assert_eq!(queue_command(PaneFocus::Cursor, "c", ctrl, true), None);
    assert!(queue_keys_armed(PaneFocus::Cursor));
    assert!(!queue_keys_armed(PaneFocus::ReasonPrompt));

    let on = |key| queue_command(PaneFocus::Cursor, key, none, true);
    assert_eq!(on("ArrowDown"), Some(QueueCommand::Move(FocusMove::Down)));
    assert_eq!(on("ArrowUp"), Some(QueueCommand::Move(FocusMove::Up)));
    assert_eq!(on("Enter"), Some(QueueCommand::Open));
    assert_eq!(on("n"), Some(QueueCommand::ToggleNeededMe));
    assert_eq!(on("G"), Some(QueueCommand::GoToNode));
    assert_eq!(on("s"), Some(QueueCommand::OpenSettings));
    assert_eq!(
        on("c"),
        Some(QueueCommand::Site(PaneCommand::Dispose(
            crate::triage::TriageState::Confirmed
        )))
    );
    assert_eq!(
        on("i"),
        Some(QueueCommand::Site(PaneCommand::PromptFor(
            crate::triage::TriageState::Ignored
        )))
    );
    assert_eq!(
        queue_command(
            PaneFocus::Cursor,
            "ArrowDown",
            KeyMods {
                shift: true,
                ..none
            },
            true
        ),
        Some(QueueCommand::ShowMore)
    );
    // With no row under the cursor, only moving and the settings jump act.
    for key in ["Enter", "c", "n", "g"] {
        assert_eq!(
            queue_command(PaneFocus::Cursor, key, none, false),
            None,
            "{key}"
        );
    }
    assert_eq!(
        queue_command(PaneFocus::Cursor, "s", none, false),
        Some(QueueCommand::OpenSettings)
    );
    assert_eq!(on("x"), None);
}

#[test]
fn the_cursor_walks_the_drawn_rows_and_stops_at_the_ends() {
    let keys = ["a", "b", "c"];
    let mut s = QueueState::default();
    s.move_cursor(&keys, FocusMove::Down);
    assert_eq!(s.cursor.as_deref(), Some("a"));
    s.move_cursor(&keys, FocusMove::Down);
    s.move_cursor(&keys, FocusMove::Down);
    s.move_cursor(&keys, FocusMove::Down);
    assert_eq!(s.cursor.as_deref(), Some("c"));
    s.move_cursor(&keys, FocusMove::Up);
    assert_eq!(s.cursor.as_deref(), Some("b"));
    let mut fresh = QueueState::default();
    fresh.move_cursor(&keys, FocusMove::Up);
    assert_eq!(
        fresh.cursor.as_deref(),
        Some("c"),
        "Up from nowhere starts at the end"
    );
    let mut gone = QueueState {
        cursor: Some("z".to_string()),
        ..QueueState::default()
    };
    gone.move_cursor(&keys, FocusMove::Down);
    assert_eq!(
        gone.cursor.as_deref(),
        Some("a"),
        "a cursor on a row no longer drawn starts over"
    );
    gone.move_cursor(&[], FocusMove::Down);
    assert_eq!(gone.cursor.as_deref(), Some("a"));
}

#[test]
fn no_queue_text_has_an_em_dash_or_a_raw_nul() {
    let v = view();
    let l = layout(
        &v,
        &QueueState {
            show_below: true,
            ..QueueState::default()
        },
    );
    let mut texts = vec![
        budget_line(&v),
        counts_line(&v),
        needed_me_line(&v.needed_me),
        empty_text(&v),
        EMPTY.to_string(),
        NEEDED_ME_HINT.to_string(),
        keys_hint(true, false).to_string(),
        keys_hint(false, true).to_string(),
        keys_hint(false, false).to_string(),
        l.footer.clone().unwrap_or_default(),
    ];
    for e in &l.entries {
        match e {
            Entry::Line(t) | Entry::OverBand(t) | Entry::BelowHeading(t) => texts.push(t.clone()),
            Entry::Row(r) => {
                texts.push(reason_cell(r));
                texts.push(node_cell(r));
                texts.push(decision_text(r));
            }
        }
    }
    for t in texts {
        assert!(!t.contains('\u{2014}') && !t.contains('\0'), "{t:?}");
    }
}

#[test]
fn the_empty_state_says_what_the_group_reviewer_did() {
    let mut v = view();
    assert!(empty_text(&v).starts_with("No escalations."));
    assert!(empty_text(&v).ends_with("The group reviewer read this run."));
    v.group_reviewed = false;
    assert!(empty_text(&v).ends_with("The group reviewer has not read this run."));
}

#[test]
fn the_detail_context_of_a_queue_row_is_read_from_the_queue_it_sits_in() {
    let v = view();
    let row = &v.escalated[0];
    let ctx = detail_context(&v, row);

    // The crumb is the row's own node path, label by label.
    let crumbs: Vec<&str> = row.node_path.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(ctx.crumbs, crumbs);
    assert!(!ctx.crumbs.is_empty());

    // The rule select holds every rule the queue holds, once, sorted, and the row's own.
    let mut rules: Vec<String> = v
        .escalated
        .iter()
        .chain(&v.below)
        .map(|r| r.site.rule_id.clone())
        .collect();
    rules.sort();
    rules.dedup();
    assert_eq!(ctx.rule_ids, rules);
    assert!(ctx.rule_ids.contains(&row.site.rule_id));

    // The merge picker's pool is the sites the queue has loaded, this row among them.
    assert_eq!(ctx.pool.len(), v.escalated.len() + v.below.len());
    assert!(ctx.pool.iter().any(|p| p.key == row.site.site_key));

    // Sub-groups to split into: the other sub-groups under the same rule the queue shows.
    let own = row.node_path.last().map(|c| c.label.clone()).unwrap();
    assert!(!ctx.subgroups.contains(&own), "not the row's own");
    let rule_node = &row.node_path[row.node_path.len() - 2].id;
    for label in &ctx.subgroups {
        assert!(
            v.escalated.iter().chain(&v.below).any(|r| {
                let n = r.node_path.len();
                n >= 2 && r.node_path[n - 2].id == *rule_node && r.node_path[n - 1].label == *label
            }),
            "{label} is a sub-group of the same rule in the queue"
        );
    }
    let mut seen = ctx.subgroups.clone();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), ctx.subgroups.len(), "each once");
}
