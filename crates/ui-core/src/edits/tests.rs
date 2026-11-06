use super::*;
use crate::test_support::review_fixtures::dataset_base;
use crate::tree::batch::summary_edit_request;
use camerata_api_types::decisions::ClientAck;
use camerata_api_types::review_view::{
    DuplicateView, HoldReason, Marker, MergedIntoView, NewSinceDecisionView, SiteRow,
};

fn rows() -> Vec<SiteRow> {
    dataset_base().rows
}

fn plain() -> SiteRow {
    rows()
        .into_iter()
        .find(|r| {
            r.markers.is_empty()
                && r.decision.is_none()
                && r.edits.is_empty()
                && r.category.is_some()
                && !r.fix.is_empty()
        })
        .expect("a plain row")
}

/// A row the engine judged a deviation from a convention.
fn deviation_row() -> SiteRow {
    let mut row = plain();
    row.kind = Some(camerata_api_types::audit::FindingKind::Deviation(
        camerata_api_types::audit::DeviationKind {
            deviating_sites: 3,
            assessed_sites: Some(40),
            convention_option_id: None,
            convention_label: "URI versioning".into(),
        },
    ));
    row
}

fn ctx() -> DetailContext {
    DetailContext {
        crumbs: vec![
            "Authorization".into(),
            "SEC-RLS-1".into(),
            "sub 3: service-role in client".into(),
        ],
        rule_ids: vec!["SEC-RLS-1".into(), "SEC-KEY-2".into()],
        subgroups: vec!["sub 1: policies".into(), "sub 2: grants".into()],
        node_decision: Some(DecisionState::Confirmed),
        pool: Vec::new(),
        pool_none: POOL_NONE_TREE,
    }
}

/// A value each field can take that differs from what `row` holds.
fn other_value(row: &SiteRow, field: EditField) -> String {
    match field {
        EditField::Headline => "A different headline".into(),
        EditField::Detail => "A different explanation".into(),
        EditField::Fix => "A different fix".into(),
        EditField::FixType => {
            if row.fix_type == "Tailored fix" {
                "Standard fix".into()
            } else {
                "Tailored fix".into()
            }
        }
        EditField::Severity => {
            if row.severity == "low" {
                "high".into()
            } else {
                "low".into()
            }
        }
        EditField::RuleId => "SEC-OTHER-9".into(),
        EditField::Kind => "defect".into(),
        EditField::Category => {
            if row.category.as_deref() == Some("performance") {
                "injection".into()
            } else {
                "performance".into()
            }
        }
        EditField::Subgroup => "A new sub-group".into(),
        EditField::Summary => "A different summary".into(),
    }
}

// ── The list of fields ────────────────────────────────────────────────────────────────────

#[test]
fn every_editable_field_has_a_control_and_round_trips() {
    // A deviation, so that moving it to `defect` is a change.
    let row = deviation_row();
    let ctx = ctx();
    assert_eq!(EditField::ALL.len(), SITE_FIELDS.len() + 1);
    for field in EditField::ALL {
        let control = control_for(field, &row, &ctx);
        match field {
            EditField::Summary => {
                // WO10's node edit covers it: the same fields list, edited at the node head.
                assert_eq!(control, Control::NodeSummary);
                let node = dataset_base()
                    .tree
                    .nodes
                    .into_iter()
                    .find(|n| n.summary.is_some() && !n.thrown_out)
                    .expect("a node with a summary");
                let req = summary_edit_request(&node, "A different summary", "why", "insp-1")
                    .expect("the node edit builds");
                assert_eq!(req.field, EditField::Summary);
                assert_eq!(req.key, node.id);
                assert!(SITE_FIELDS.iter().all(|f| *f != EditField::Summary));
                continue;
            }
            EditField::Subgroup => assert_eq!(control, Control::Split),
            EditField::Headline => assert_eq!(control, Control::Line),
            EditField::Detail | EditField::Fix => assert_eq!(control, Control::Multi),
            EditField::RuleId => assert!(matches!(control, Control::Combo(_))),
            _ => assert!(matches!(control, Control::Select(_)), "{field:?}"),
        }
        // The field has a label and a chord letter, and the letter picks the field back.
        assert!(!field_label(field).is_empty());
        let letter = chord_letter(field).unwrap_or_else(|| panic!("{field:?} has no letter"));
        assert_eq!(
            field_for_chord(&letter.to_string()),
            Some(field),
            "{letter}"
        );
        assert_eq!(
            field_for_chord(&letter.to_ascii_lowercase().to_string()),
            Some(field)
        );

        // Every choice a select offers is a value the field accepts; a typed value is built
        // into the request the edits route takes, with its reason, and survives the wire.
        if let Control::Select(options) | Control::Combo(options) = &control {
            for o in options {
                // The row's own value is offered too (a deviation cannot be typed), and picking
                // it is refused as no change.
                if o.value == current_value(&row, field, &ctx) {
                    continue;
                }
                assert_eq!(value_problem(field, &o.value), None, "{field:?} {o:?}");
            }
        }
        let new = other_value(&row, field);
        let req = edit_request(
            &row,
            field,
            &new,
            "  the engine was wrong  ",
            &ctx,
            "insp-1",
        )
        .unwrap_or_else(|e| panic!("{field:?}: {}", e.message()));
        assert_eq!(req.field, field);
        assert_eq!(req.key, row.site_key);
        assert_eq!(req.new, new);
        assert_eq!(req.reason, "the engine was wrong");
        let wire = serde_json::to_string(&req).unwrap();
        let back: camerata_api_types::edits::NewEditReq = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, req);
    }
}

#[test]
fn the_chord_letters_are_unique_and_the_spec_ones_are_kept() {
    let letters: Vec<char> = SITE_FIELDS
        .iter()
        .filter_map(|f| chord_letter(*f))
        .collect();
    assert_eq!(letters.len(), SITE_FIELDS.len());
    let mut sorted = letters.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), letters.len(), "no letter is used twice");
    // Spec section 8: H, D, X fix, S severity, R rule, K kind, G category, U sub-group.
    for (letter, field) in [
        ('H', EditField::Headline),
        ('D', EditField::Detail),
        ('X', EditField::Fix),
        ('S', EditField::Severity),
        ('R', EditField::RuleId),
        ('K', EditField::Kind),
        ('G', EditField::Category),
        ('U', EditField::Subgroup),
    ] {
        assert_eq!(chord_letter(field), Some(letter));
    }
    assert_eq!(field_for_chord("z"), None);
    assert_eq!(field_for_chord("HH"), None);
    assert_eq!(chord_press("Escape"), ChordPress::Cancel);
    assert_eq!(chord_press("h"), ChordPress::Field(EditField::Headline));
    assert_eq!(chord_press("q"), ChordPress::Ignore);
    assert!(chord_hint().contains("S severity"));
}

#[test]
fn the_detail_keys_are_e_m_a_and_u_in_either_case() {
    assert_eq!(detail_key("e"), Some(DetailKey::Edit));
    assert_eq!(detail_key("E"), Some(DetailKey::Edit));
    assert_eq!(detail_key("m"), Some(DetailKey::Merge));
    assert_eq!(detail_key("a"), Some(DetailKey::Add));
    assert_eq!(detail_key("u"), Some(DetailKey::Split));
    assert_eq!(detail_key("c"), None);
    assert_eq!(detail_key("Enter"), None);
}

// ── The edit request ──────────────────────────────────────────────────────────────────────

#[test]
fn edit_requires_a_reason_and_posts_to_the_edits_route() {
    let row = plain();
    let ctx = ctx();
    for blank in ["", "   ", "\n"] {
        assert_eq!(
            edit_request(&row, EditField::Headline, "New", blank, &ctx, "insp-1"),
            Err(EditRefusal::ReasonRequired)
        );
    }
    let req = edit_request(&row, EditField::Headline, " New ", " why ", &ctx, "insp-1").unwrap();
    assert_eq!(req.key, row.site_key, "keyed by the site");
    assert_eq!(req.new, "New", "the value is trimmed");
    assert_eq!(req.reason, "why");
    assert_eq!(req.run_id, "insp-1");
    // The old value and the fingerprint are the server's to work out.
    assert!(req.by.is_empty());
}

#[test]
fn an_edit_that_changes_nothing_or_names_an_impossible_value_is_refused() {
    let row = plain();
    let ctx = ctx();
    let same = row.headline.clone();
    assert_eq!(
        edit_request(&row, EditField::Headline, &same, "why", &ctx, "r"),
        Err(EditRefusal::Unchanged)
    );
    assert!(matches!(
        edit_request(&row, EditField::Headline, "  ", "why", &ctx, "r"),
        Err(EditRefusal::Invalid(_))
    ));
    assert!(matches!(
        edit_request(&row, EditField::Severity, "urgent", "why", &ctx, "r"),
        Err(EditRefusal::Invalid(m)) if m.contains("critical, high, medium or low")
    ));
    assert!(matches!(
        edit_request(&row, EditField::FixType, "Quick fix", "why", &ctx, "r"),
        Err(EditRefusal::Invalid(_))
    ));
    // A deviation carries numbers the engine measured, so it cannot be typed.
    assert!(matches!(
        edit_request(&row, EditField::Kind, "deviation", "why", &ctx, "r"),
        Err(EditRefusal::Invalid(m)) if m.contains("convention numbers")
    ));
    // A blank category clears it, which is a change on a row that has one.
    let cleared = edit_request(&row, EditField::Category, "", "why", &ctx, "r").unwrap();
    assert_eq!(cleared.new, "");
    // A reason is asked for last, so an impossible value is named first.
    assert!(matches!(
        edit_request(&row, EditField::Severity, "urgent", "", &ctx, "r"),
        Err(EditRefusal::Invalid(_))
    ));
}

#[test]
fn a_select_offers_the_current_value_even_when_it_is_off_the_list() {
    let mut row = plain();
    row.severity = "info".into();
    row.category = Some("custom-label".into());
    let Control::Select(sev) = control_for(EditField::Severity, &row, &ctx()) else {
        panic!("a select");
    };
    assert_eq!(sev[0].value, "info", "the row's own severity is first");
    assert_eq!(sev.len(), 5);
    let Control::Select(cat) = control_for(EditField::Category, &row, &ctx()) else {
        panic!("a select");
    };
    assert!(cat.iter().any(|o| o.value == "custom-label"));
    assert!(cat.iter().any(|o| o.value.is_empty()), "and a way to clear");
    // The rule select lists the row's own rule once, then the run's.
    let Control::Combo(rules) = control_for(EditField::RuleId, &row, &ctx()) else {
        panic!("a combo");
    };
    let values: Vec<&str> = rules.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(values.iter().filter(|v| **v == row.rule_id).count(), 1);
    assert!(values.contains(&"SEC-KEY-2"));
}

#[test]
fn the_kind_select_moves_a_row_to_defect_and_names_a_deviation_as_current() {
    let row = deviation_row();
    let Control::Select(options) = control_for(EditField::Kind, &row, &ctx()) else {
        panic!("a select");
    };
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].value, "deviation");
    assert!(options[0].label.contains("current"));
    assert_eq!(options[1].value, "defect");
    // Choosing the current deviation is no change, and cannot be posted as a bare tag.
    assert!(edit_request(&row, EditField::Kind, "deviation", "why", &ctx(), "r").is_err());
    assert!(edit_request(&row, EditField::Kind, "defect", "why", &ctx(), "r").is_ok());
}

// ── An edited and a stale field ───────────────────────────────────────────────────────────

#[test]
fn an_edited_field_shows_the_applied_edit_and_a_stale_one_applies_nothing() {
    let all = rows();
    let edited = all
        .iter()
        .find(|r| r.edits.iter().any(|e| !e.stale))
        .expect("an edited row");
    let e = edited.edits.iter().find(|e| !e.stale).unwrap();
    let state = field_state(edited, e.field, &ctx());
    assert_eq!(state.applied.as_ref().map(|a| &a.id), Some(&e.id));
    assert!(state.stale.is_none());

    let stale_row = all
        .iter()
        .find(|r| r.edits.iter().any(|e| e.stale))
        .expect("a row with a stale edit");
    let s = stale_row.edits.iter().find(|e| e.stale).unwrap();
    let state = field_state(stale_row, s.field, &ctx());
    assert_eq!(state.stale.as_ref().map(|a| &a.id), Some(&s.id));
    assert_eq!(
        state.applied.as_ref().map(|a| a.id.clone()),
        stale_row
            .edits
            .iter()
            .rfind(|e| e.field == s.field && !e.stale)
            .map(|e| e.id.clone()),
        "a stale edit is never the applied one"
    );
    assert!(stale_edit_line(s).starts_with("Your edit (stale): "));
    assert!(stale_edit_line(s).ends_with(&s.new));
    // The field's value is the row's own, whatever the stale edit says.
    assert_eq!(state.value, current_value(stale_row, s.field, &ctx()));
}

#[test]
fn stale_chip_shows_the_prior_value_and_applies_nothing() {
    let stale_row = rows()
        .into_iter()
        .find(|r| r.edits.iter().any(|e| e.stale))
        .expect("a row with a stale edit");
    let s = stale_row.edits.iter().find(|e| e.stale).unwrap();
    let tip = stale_edit_tip(s);
    assert!(tip.contains(&s.old) && tip.contains(&s.new), "{tip}");
    assert!(tip.contains("nothing was applied"));
    // The stale edit changes no value the row shows.
    for f in SITE_FIELDS {
        let st = field_state(&stale_row, f, &ctx());
        if let Some(applied) = &st.applied {
            assert!(!applied.stale);
        }
    }
}

#[test]
fn an_edit_hover_names_the_old_value_the_reason_who_and_when() {
    let e = EditView {
        id: "edit-000001".into(),
        field: EditField::Headline,
        old: "Old headline".into(),
        new: "New headline".into(),
        reason: "it named the wrong table".into(),
        by: "zach".into(),
        at: "2026-11-04T09:00:00Z".into(),
        stale: false,
        current_value: String::new(),
        reapplied_from: None,
    };
    let tip = edit_tip(&e);
    assert!(tip.contains("Was: Old headline"), "{tip}");
    assert!(tip.contains("Why: it named the wrong table"));
    assert!(tip.contains("By zach on 11-04"));
    let blank = EditView {
        old: String::new(),
        reason: String::new(),
        by: String::new(),
        ..e
    };
    assert_eq!(edit_tip(&blank), "Was: (empty)");
}

// ── Split ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_split_to_a_new_sub_group_needs_a_label_and_a_reason_and_names_where_it_went() {
    let row = plain();
    let ctx = ctx();
    let mode = reduce(DetailMode::Viewing, &DetailAction::BeginSplit, &row, &ctx);
    let DetailMode::Split(d) = &mode else {
        panic!("{mode:?}");
    };
    assert_eq!(
        d.choice,
        SplitChoice::Existing("sub 1: policies".into()),
        "the first other sub-group of the rule is offered"
    );
    // Moving to a new sub-group.
    let mode = reduce(mode, &DetailAction::Split(SplitChoice::New), &row, &ctx);
    assert_eq!(
        commit(&mode, &row, &ctx, "r"),
        Err("A new sub-group needs a label.".to_string())
    );
    let mode = reduce(
        mode,
        &DetailAction::Text("test fixtures".into()),
        &row,
        &ctx,
    );
    assert_eq!(
        commit(&mode, &row, &ctx, "r"),
        Err(EditRefusal::ReasonRequired.message()),
        "the reason comes next"
    );
    let mode = reduce(
        mode,
        &DetailAction::Reason("different cause".into()),
        &row,
        &ctx,
    );
    let Ok(DetailWrite::Edit { req, done }) = commit(&mode, &row, &ctx, "insp-1") else {
        panic!("a split is an edit");
    };
    assert_eq!(req.field, EditField::Subgroup);
    assert_eq!(req.new, "test fixtures");
    assert_eq!(req.reason, "different cause");
    assert!(done.contains("\u{201c}test fixtures\u{201d}"), "{done}");
    assert!(done.contains("member count"));
    // Moving to a sub-group the site already sits in changes nothing.
    let here = ctx.crumbs[2].clone();
    let mode = reduce(
        DetailMode::Split(SplitDraft {
            choice: SplitChoice::Existing(here),
            new_label: String::new(),
            reason: "r".into(),
            error: None,
        }),
        &DetailAction::Reason("r".into()),
        &row,
        &ctx,
    );
    assert_eq!(
        commit(&mode, &row, &ctx, "r"),
        Err("The site is already in that sub-group.".to_string())
    );
}

#[test]
fn a_sub_group_edit_is_a_split_and_a_summary_is_not_a_site_edit() {
    let row = plain();
    let mode = reduce(
        DetailMode::Viewing,
        &DetailAction::BeginEdit(EditField::Subgroup),
        &row,
        &ctx(),
    );
    assert!(matches!(mode, DetailMode::Split(_)));
    let mode = reduce(
        DetailMode::Viewing,
        &DetailAction::BeginEdit(EditField::Summary),
        &row,
        &ctx(),
    );
    assert_eq!(mode, DetailMode::Viewing);
}

// ── Merge ─────────────────────────────────────────────────────────────────────────────────

fn pool_of(rows: &[SiteRow]) -> Vec<PoolSite> {
    rows.iter().map(PoolSite::of).collect()
}

#[test]
fn merge_keeps_the_row() {
    let all = rows();
    let row = all
        .iter()
        .find(|r| r.merged_into.is_none() && r.decision.is_none())
        .unwrap();
    let target = all
        .iter()
        .find(|r| r.site_key != row.site_key && r.merged_into.is_none())
        .unwrap();
    let ctx = DetailContext {
        pool: pool_of(&all),
        ..ctx()
    };
    // The merge is a site-scope decision with the target's key and a reason; no edit, no delete.
    let mode = reduce(DetailMode::Viewing, &DetailAction::BeginMerge, row, &ctx);
    let mode = reduce(
        mode,
        &DetailAction::Pick(target.site_key.clone()),
        row,
        &ctx,
    );
    assert_eq!(
        commit(&mode, row, &ctx, "r"),
        Err(MergeRefusal::ReasonRequired.message().to_string())
    );
    let mode = reduce(mode, &DetailAction::Reason("same helper".into()), row, &ctx);
    let Ok(DetailWrite::Decision { req, done }) = commit(&mode, row, &ctx, "insp-1") else {
        panic!("a merge is a decision");
    };
    assert_eq!(req.scope, Scope::Site);
    assert_eq!(req.key, row.site_key, "the merged row is the one decided");
    assert_eq!(
        req.state,
        DecisionState::MergedInto(target.site_key.clone())
    );
    assert_eq!(req.reason, "same helper");
    assert!(
        done.ends_with("The row stays in the tree."),
        "the bar says the row remains: {done}"
    );
    assert!(done.contains(&format!("{}:{}", target.path, target.line)));
    // The merged row reads "Merged into path:line" in its Decision line, not a removal.
    let mut merged = row.clone();
    merged.decision = Some(DecisionView {
        id: "dec-1".into(),
        state: DecisionState::MergedInto(target.site_key.clone()),
        reason: "same helper".into(),
        scope: Scope::Site,
        inherited_from: None,
        decided_by: "zach".into(),
        decided_at: "2026-11-04T09:00:00Z".into(),
        needed_me: None,
        confirmed_by_client: None,
    });
    merged.merged_into = Some(MergedIntoView {
        key: target.site_key.clone(),
        label: format!("{}:{}", target.path, target.line),
    });
    let line = decision_line(&merged, &ctx);
    assert_eq!(
        line.text,
        format!("Merged into {}:{}", target.path, target.line)
    );
    assert_eq!(line.tone, LineTone::Decided);
}

#[test]
fn the_merge_picker_lists_the_duplicate_first_then_a_search_over_path_line_and_rule() {
    let all = rows();
    let mut row = all[0].clone();
    let other = all
        .iter()
        .find(|r| r.site_key != row.site_key && r.rule_id != row.rule_id)
        .unwrap()
        .clone();
    row.possible_duplicate_of = Some(DuplicateView {
        key: other.site_key.clone(),
        hint: "same line, different family".into(),
        rule_id: other.rule_id.clone(),
        path: other.path.clone(),
        line: other.line,
    });
    let ctx = DetailContext {
        pool: pool_of(&all),
        ..ctx()
    };
    let list = merge_candidates(&row, &ctx, "");
    assert_eq!(list[0].key, other.site_key, "the link comes first");
    assert_eq!(list[0].note.as_deref(), Some("possible duplicate"));
    assert!(list.len() <= MERGE_LIST_MAX);
    assert!(list.iter().all(|c| c.key != row.site_key), "never itself");
    assert_eq!(
        list.iter().filter(|c| c.key == other.site_key).count(),
        1,
        "the link is not listed twice"
    );

    // A search matches the path and line, or the rule, in either case.
    let target = all
        .iter()
        .find(|r| r.site_key != row.site_key && r.site_key != other.site_key)
        .unwrap();
    let by_path = merge_candidates(&row, &ctx, &format!("{}:{}", target.path, target.line));
    assert!(by_path.iter().any(|c| c.key == target.site_key));
    let by_rule = merge_candidates(&row, &ctx, &target.rule_id.to_lowercase());
    assert!(by_rule.iter().any(|c| c.key == target.site_key));
    let nothing = merge_candidates(&row, &ctx, "no such place anywhere:9999");
    assert_eq!(
        nothing.len(),
        1,
        "only the linked duplicate survives a search it does not match"
    );

    // A row merged away is not a target.
    let mut pool = pool_of(&all);
    if let Some(p) = pool.iter_mut().find(|p| p.key == target.site_key) {
        p.merged = true;
    }
    let ctx = DetailContext { pool, ..ctx };
    let again = merge_candidates(&row, &ctx, &format!("{}:{}", target.path, target.line));
    assert!(again.iter().all(|c| c.key != target.site_key));
}

#[test]
fn a_merge_needs_a_target_a_reason_and_a_different_site() {
    let row = plain();
    assert_eq!(
        merge_request(&row, None, "r", "run").unwrap_err(),
        MergeRefusal::NoTarget
    );
    assert_eq!(
        merge_request(&row, Some(""), "r", "run").unwrap_err(),
        MergeRefusal::NoTarget
    );
    assert_eq!(
        merge_request(&row, Some(&row.site_key), "r", "run").unwrap_err(),
        MergeRefusal::Itself
    );
    assert_eq!(
        merge_request(&row, Some("other"), "  ", "run").unwrap_err(),
        MergeRefusal::ReasonRequired
    );
    let req = merge_request(&row, Some("other"), " same cause ", "run").unwrap();
    assert_eq!(req.reason, "same cause");
    assert_eq!(req.run_id, "run");
}

#[test]
fn the_merge_form_opens_on_the_linked_duplicate() {
    let mut row = plain();
    row.possible_duplicate_of = Some(DuplicateView {
        key: "dup-key".into(),
        hint: "h".into(),
        rule_id: "SEC-KEY-2".into(),
        path: "web/env.ts".into(),
        line: 3,
    });
    let mode = reduce(DetailMode::Viewing, &DetailAction::BeginMerge, &row, &ctx());
    let DetailMode::Merge(d) = mode else {
        panic!("a merge form");
    };
    assert_eq!(d.target.as_deref(), Some("dup-key"));
}

// ── Add a finding ─────────────────────────────────────────────────────────────────────────

fn filled(row: &SiteRow) -> AddDraft {
    let mut d = AddDraft::from_row(row);
    d.headline = "Key shipped to the browser".into();
    d.detail = "The service role key is in a public variable.".into();
    d.reason = "the engine missed the second copy".into();
    d.snippet = Snippet::Loaded("const k = import.meta.env.VITE_KEY;".into());
    d
}

#[test]
fn the_add_form_is_prefilled_from_the_site_and_builds_the_request_the_server_takes() {
    let row = plain();
    let draft = filled(&row);
    assert_eq!(draft.repo, row.repo);
    assert_eq!(draft.path, row.path);
    assert_eq!(draft.line_number(), Some(row.line));
    assert_eq!(draft.rule_id, row.rule_id);
    let req = add_request(&draft).unwrap();
    assert_eq!(req.repo, row.repo);
    assert_eq!(req.line, row.line);
    assert_eq!(req.snippet, "const k = import.meta.env.VITE_KEY;");
    assert_eq!(
        req.kind,
        Some(camerata_api_types::audit::FindingKind::Defect)
    );
    assert_eq!(req.reason, "the engine missed the second copy");
    assert!(req.by.is_empty());
    // The wire body parses as the server's AddFindingReq.
    let wire = serde_json::to_value(&req).unwrap();
    assert_eq!(wire["kind"]["kind"], "defect");
}

#[test]
fn an_incomplete_add_form_says_what_is_missing() {
    let row = plain();
    let missing = |edit: &dyn Fn(&mut AddDraft)| {
        let mut d = filled(&row);
        edit(&mut d);
        add_request(&d).unwrap_err()
    };
    assert!(missing(&|d| d.path.clear()).contains("a repo and a file path"));
    assert!(missing(&|d| d.line = "x".into()).contains("a line number"));
    assert!(missing(&|d| d.rule_id.clear()).contains("a rule id"));
    assert!(missing(&|d| d.headline.clear()).contains("a headline and an explanation"));
    assert!(missing(&|d| d.reason.clear()).contains("a reason"));
    assert!(missing(&|d| d.snippet = Snippet::NotLoaded).contains("Load the code"));
    assert!(missing(&|d| d.snippet = Snippet::Loading).contains("still loading"));
    assert_eq!(
        missing(&|d| d.snippet = Snippet::Failed("That file is not in the repo.".into())),
        "That file is not in the repo."
    );
}

#[test]
fn changing_where_the_finding_is_forgets_the_snippet_shown() {
    let row = plain();
    let ctx = ctx();
    let mode = DetailMode::Add(Box::new(filled(&row)));
    let mode = reduce(
        mode,
        &DetailAction::Add(AddField::Headline, "Another".into()),
        &row,
        &ctx,
    );
    let DetailMode::Add(d) = &mode else {
        panic!();
    };
    assert!(
        matches!(d.snippet, Snippet::Loaded(_)),
        "text edits keep it"
    );
    let mode = reduce(
        mode,
        &DetailAction::Add(AddField::Line, "999".into()),
        &row,
        &ctx,
    );
    let DetailMode::Add(d) = &mode else {
        panic!();
    };
    assert_eq!(d.snippet, Snippet::NotLoaded, "another line, another code");
    assert_eq!(d.line_number(), Some(999));
}

#[test]
fn add_refused_shows_the_gates_reason() {
    // The evidence check did not find the quoted code: the server adds the row, holds it, and
    // answers with the check's own words.
    let held = FindingAdded {
        key: "k".into(),
        evidence_verified: false,
        evidence_note: "The quoted code was not found within 3 lines of line 40.".into(),
    };
    let outcome = added_outcome(&held);
    assert_eq!(
        outcome,
        AddedOutcome::Held("The quoted code was not found within 3 lines of line 40.".into())
    );
    let said = added_message(&outcome);
    assert!(
        said.contains("was not found within 3 lines of line 40."),
        "{said}"
    );
    assert!(said.contains("until you decide it"), "{said}");
    // A check that gave no words still gives a reason.
    let blank = FindingAdded {
        evidence_note: String::new(),
        ..held
    };
    assert!(matches!(added_outcome(&blank), AddedOutcome::Held(w) if w.contains("did not find")));
    let ok = FindingAdded {
        key: "k".into(),
        evidence_verified: true,
        evidence_note: String::new(),
    };
    assert_eq!(added_outcome(&ok), AddedOutcome::Verified);
    assert!(added_message(&AddedOutcome::Verified).contains("edit mark"));
    // A refusal shown in the open form keeps what was typed.
    let row = plain();
    let mode = DetailMode::Add(Box::new(filled(&row))).with_error("Refused.".into());
    assert_eq!(mode.error(), Some("Refused."));
    let DetailMode::Add(d) = &mode else {
        panic!();
    };
    assert_eq!(d.headline, "Key shipped to the browser");
}

#[test]
fn the_snippet_is_the_text_of_the_line_in_the_sources_window() {
    let lines: Vec<String> = ["a", "b", "c"].iter().map(ToString::to_string).collect();
    assert_eq!(snippet_at_line(10, &lines, 11), Some("b".into()));
    assert_eq!(snippet_at_line(10, &lines, 9), None);
    assert_eq!(snippet_at_line(10, &lines, 13), None);
    assert_eq!(
        snippet_at_line(1, &["".to_string()], 1),
        None,
        "a blank line is no evidence"
    );
}

// ── Client acknowledgement ────────────────────────────────────────────────────────────────

#[test]
fn client_ack_only_on_ignored_rows() {
    let all = rows();
    let mut needed = 0;
    let mut acked = 0;
    for row in &all {
        let ignored = row
            .decision
            .as_ref()
            .is_some_and(|d| d.state == DecisionState::Ignored);
        match ack_state(row) {
            AckState::NotApplicable => {
                assert!(!ignored, "an Ignored row has a client line");
                assert_eq!(ack_line(row), None);
                assert_eq!(ack_target(row), None);
                assert_eq!(
                    client_ack_request(row, "cto@acme.test", "ok").unwrap_err(),
                    AckRefusal::NotIgnored
                );
            }
            AckState::Needed { decision_id } => {
                assert!(ignored);
                needed += 1;
                assert_eq!(ack_line(row).as_deref(), Some("Needs client confirmation"));
                let (id, req) = client_ack_request(row, " cto@acme.test ", " ok ").unwrap();
                assert_eq!(id, decision_id);
                assert_eq!(req.by, "cto@acme.test");
                assert_eq!(req.note, "ok");
            }
            AckState::Acknowledged { .. } => {
                assert!(ignored);
                acked += 1;
                // Already acknowledged: it cannot be acknowledged twice.
                assert_eq!(ack_target(row), None);
                assert_eq!(
                    client_ack_request(row, "cto@acme.test", "").unwrap_err(),
                    AckRefusal::NotIgnored
                );
            }
        }
    }
    assert!(
        needed > 0 && acked > 0,
        "the fixtures hold both ({needed}, {acked})"
    );
}

#[test]
fn an_acknowledgement_names_who_and_the_line_reads_accepted_risk_with_the_reason() {
    let all = rows();
    let row = all
        .iter()
        .find(|r| {
            r.decision.as_ref().is_some_and(|d| {
                d.state == DecisionState::Ignored && d.confirmed_by_client.is_none()
            })
        })
        .unwrap();
    assert_eq!(
        client_ack_request(row, "  ", "n").unwrap_err(),
        AckRefusal::WhoRequired
    );
    let mut acked = row.clone();
    if let Some(d) = acked.decision.as_mut() {
        d.reason = "behind the corporate proxy".into();
        d.confirmed_by_client = Some(ClientAck {
            by: "cto@acme.test".into(),
            at: "2026-11-05T10:00:00Z".into(),
            note: "signed off in the call".into(),
        });
    }
    assert_eq!(
        ack_line(&acked).as_deref(),
        Some("Accepted risk: behind the corporate proxy"),
        "exactly as the exports print it"
    );
    assert_eq!(
        ack_detail(&acked).as_deref(),
        Some("Confirmed by cto@acme.test on 11-05: signed off in the call")
    );
}

#[test]
fn the_ack_form_opens_only_for_a_row_that_needs_one() {
    let all = rows();
    let needs = all
        .iter()
        .find(|r| matches!(ack_state(r), AckState::Needed { .. }))
        .unwrap();
    let mode = reduce(DetailMode::Viewing, &DetailAction::BeginAck, needs, &ctx());
    assert!(matches!(mode, DetailMode::Ack(_)));
    let other = all
        .iter()
        .find(|r| matches!(ack_state(r), AckState::NotApplicable))
        .unwrap();
    let mode = reduce(DetailMode::Viewing, &DetailAction::BeginAck, other, &ctx());
    assert_eq!(mode, DetailMode::Viewing);
    // Who first, then a note; the commit builds the request on the decision.
    let mode = reduce(
        reduce(DetailMode::Viewing, &DetailAction::BeginAck, needs, &ctx()),
        &DetailAction::Text("cto@acme.test".into()),
        needs,
        &ctx(),
    );
    let mode = reduce(mode, &DetailAction::Reason("ok".into()), needs, &ctx());
    let Ok(DetailWrite::Ack { decision_id, req }) = commit(&mode, needs, &ctx(), "r") else {
        panic!("an ack");
    };
    assert_eq!(Some(decision_id), ack_target(needs));
    assert_eq!(
        (req.by.as_str(), req.note.as_str()),
        ("cto@acme.test", "ok")
    );
}

// ── Stale, moved, new since decision ──────────────────────────────────────────────────────

#[test]
fn a_stale_decision_reads_unresolved_with_what_it_was_and_offers_reapply_and_discard() {
    let row = rows()
        .into_iter()
        .find(|r| r.stale_decision.is_some() && r.decision.is_none())
        .expect("a stale decision row");
    let stale = row.stale_decision.clone().unwrap();
    let line = decision_line(&row, &ctx());
    assert!(
        line.text.starts_with("Unresolved \u{b7} stale: was "),
        "{}",
        line.text
    );
    assert!(line.text.contains(decision_label(&stale.state)));
    assert!(line.text.contains(&short_date(&stale.decided_at)));
    assert!(line.text.contains(&stale.reason), "{}", line.text);
    assert_eq!(line.tone, LineTone::Warning);
    assert_eq!(line.actions, vec![LineAction::Reapply, LineAction::Discard]);
}

#[test]
fn stale_reapply_posts_a_new_record_and_discard_revokes() {
    let row = rows()
        .into_iter()
        .find(|r| {
            r.stale_decision
                .as_ref()
                .is_some_and(|d| !matches!(d.state, DecisionState::MergedInto(_)))
                && r.decision.is_none()
        })
        .expect("a stale decision row");
    let stale = row.stale_decision.clone().unwrap();
    // Reapply: a NEW site-scope decision on the row's own key, with the same state and reason.
    let req = reapply_decision_request(&row, "insp-2").unwrap();
    assert_eq!(req.scope, Scope::Site);
    assert_eq!(req.key, row.site_key);
    assert_eq!(req.state, stale.state);
    assert_eq!(req.reason, stale.reason);
    assert_eq!(req.run_id, "insp-2");
    assert!(
        reapply_done_message(&row).ends_with("reapplied."),
        "{}",
        reapply_done_message(&row)
    );
    // Discard: the old record is revoked by its id (the controller posts the revoke); the
    // row's own decision stays Unresolved.
    assert!(!stale.id.is_empty());
    assert!(row.decision.is_none());
    // A stale merge is not carried over: its target belongs to the old run.
    let mut merge = row.clone();
    if let Some(d) = merge.stale_decision.as_mut() {
        d.state = DecisionState::MergedInto("old-target".into());
    }
    assert_eq!(reapply_decision_request(&merge, "insp-2"), None);
    // A row with nothing stale has nothing to reapply.
    assert_eq!(reapply_decision_request(&plain(), "insp-2"), None);
}

#[test]
fn new_since_decision_says_when_and_how_many_others_and_applies_nothing() {
    let row = rows()
        .into_iter()
        .find(|r| r.new_since_decision.is_some() && r.decision.is_none())
        .expect("a new-since row");
    let n = row.new_since_decision.clone().unwrap();
    let line = decision_line(&row, &ctx());
    assert!(
        line.text
            .starts_with("Unresolved \u{b7} new since you confirmed this group on "),
        "{}",
        line.text
    );
    assert!(line.text.contains(&short_date(&n.decided_at)));
    assert!(line.text.ends_with(&format!("({} others)", n.covered)) || n.covered == 1);
    assert!(line.actions.is_empty(), "nothing applies by itself");
    // The verb follows the group decision's own state.
    let ignored = DetailContext {
        node_decision: Some(DecisionState::Ignored),
        ..ctx()
    };
    assert!(decision_line(&row, &ignored)
        .text
        .contains("you ignored this group"));
    // One other reads singular.
    let mut one = row.clone();
    one.new_since_decision = Some(NewSinceDecisionView { covered: 1, ..n });
    assert!(decision_line(&one, &ctx()).text.ends_with("(1 other)"));
}

#[test]
fn a_decided_row_shows_its_state_and_reason_and_whether_it_was_inherited() {
    let row = rows()
        .into_iter()
        .find(|r| {
            r.decision
                .as_ref()
                .is_some_and(|d| d.inherited_from.is_some() && !d.reason.is_empty())
        })
        .expect("an inherited decision");
    let line = decision_line(&row, &ctx());
    assert_eq!(line.tone, LineTone::Decided);
    assert!(line.inherited);
    let d = row.decision.as_ref().unwrap();
    assert!(line.text.starts_with(decision_label(&d.state)));
    assert!(line.text.contains(&d.reason));
    assert_eq!(decision_line(&plain(), &ctx()).text, "Unresolved");
    assert_eq!(decision_line(&plain(), &ctx()).tone, LineTone::Unresolved);
}

#[test]
fn a_held_row_says_why_and_how_to_release_it() {
    let mut row = plain();
    assert_eq!(held_line(&row), None);
    row.held = Some(HoldReason::EvidenceUnverified);
    row.markers.push(Marker::Held);
    assert_eq!(
        held_line(&row).as_deref(),
        Some("Held from export: evidence unverified. Decide it to release.")
    );
}

// ── The head and the stage lines ──────────────────────────────────────────────────────────

#[test]
fn the_stage_lines_write_a_disagreement_beside_stage_1_and_never_over_it() {
    let row = rows()
        .into_iter()
        .find(|r| {
            r.stage2
                .as_ref()
                .is_some_and(|s| !s.disagreements.is_empty())
        })
        .expect("a row where stage 2 disagrees");
    let s2 = row.stage2.clone().unwrap();
    if let Some(d) = s2
        .disagreements
        .iter()
        .find(|d| d.field == DisagreementField::Severity)
    {
        let note = severity_note(&row).expect("a severity note");
        assert!(note.starts_with(&format!(
            "stage 1 said {}; stage 2 disagrees: {}",
            d.stage1, d.mine
        )));
    }
    // Stage 1's own severity is untouched by it.
    assert!(!row.stage1.severity.is_empty());
    let l1 = stage1_line(&row);
    if !row.stage1.real.is_empty() {
        let l1 = l1.expect("stage 1 line");
        assert!(l1.contains(&format!("real {}", row.stage1.real)));
    }
    assert_eq!(
        stage1_line(&SiteRow {
            stage1: Default::default(),
            ..row.clone()
        }),
        None
    );
    assert_eq!(severity_note(&plain()), None);
}

#[test]
fn the_stage_2_line_names_the_escalation_and_the_outlier() {
    let all = rows();
    let esc = all
        .iter()
        .find(|r| r.markers.contains(&Marker::Escalated) && r.stage2.is_some())
        .unwrap();
    let line = stage2_line(esc).expect("a line");
    assert!(line.starts_with("escalated"), "{line}");
    assert!(line.contains(", rank "), "{line}");
    let outlier = all
        .iter()
        .find(|r| {
            r.stage2.as_ref().is_some_and(|s| {
                s.outlier_reason.is_some() && s.state == EscalationState::NotACandidate
            })
        })
        .unwrap();
    let line = stage2_line(outlier).expect("a line");
    assert!(line.starts_with("outlier: "), "{line}");
    let mut none = plain();
    none.stage2 = None;
    assert_eq!(stage2_line(&none), None);
}

#[test]
fn also_flagged_by_and_the_possible_duplicate_are_lines_only_when_there_is_something() {
    let mut row = plain();
    row.flagged_by.clear();
    row.possible_duplicate_of = None;
    assert_eq!(flagged_by_line(&row), None);
    assert_eq!(duplicate_line(&row), None);
    let flagged = rows()
        .into_iter()
        .find(|r| !r.flagged_by.is_empty())
        .expect("a flagged row");
    let line = flagged_by_line(&flagged).unwrap();
    for f in &flagged.flagged_by {
        assert!(line.contains(&format!("({}:{})", f.path, f.line)), "{line}");
    }
    row.possible_duplicate_of = Some(DuplicateView {
        key: "k".into(),
        hint: "same line, different family".into(),
        rule_id: "SEC-KEY-2".into(),
        path: "web/env.ts".into(),
        line: 3,
    });
    assert_eq!(
        duplicate_line(&row),
        Some((
            "SEC-KEY-2 at web/env.ts:3".to_string(),
            "same line, different family".to_string()
        ))
    );
}

#[test]
fn the_review_level_reads_in_words_and_a_held_row_has_none() {
    assert_eq!(
        review_level_text(Some(&ReviewLevel::PersonallyReviewed)).as_deref(),
        Some("review level: personally reviewed")
    );
    assert!(
        review_level_text(Some(&ReviewLevel::ReviewedAsPattern("n".into())))
            .unwrap()
            .contains("pattern")
    );
    assert!(review_level_text(Some(&ReviewLevel::EngineVerified))
        .unwrap()
        .contains("engine verified"));
    assert_eq!(review_level_text(None), None);
}

// ── Forms ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn an_edit_form_opens_on_the_current_value_and_commits_through_the_reason() {
    let row = plain();
    let ctx = ctx();
    let mode = reduce(
        DetailMode::Viewing,
        &DetailAction::BeginEdit(EditField::Headline),
        &row,
        &ctx,
    );
    let DetailMode::Edit(d) = &mode else {
        panic!("{mode:?}");
    };
    assert_eq!(d.value, row.headline);
    assert!(mode.is_form());
    // Nothing changed yet: the commit says so.
    assert_eq!(
        commit(&mode, &row, &ctx, "r"),
        Err(EditRefusal::Unchanged.message())
    );
    let mode = reduce(mode, &DetailAction::Text("Better".into()), &row, &ctx);
    assert_eq!(
        commit(&mode, &row, &ctx, "r"),
        Err(EditRefusal::ReasonRequired.message())
    );
    let mode = reduce(mode, &DetailAction::Reason("clearer".into()), &row, &ctx);
    let Ok(DetailWrite::Edit { req, done }) = commit(&mode, &row, &ctx, "insp-1") else {
        panic!("an edit");
    };
    assert_eq!(
        (req.new.as_str(), req.reason.as_str()),
        ("Better", "clearer")
    );
    assert_eq!(done, "Headline edited.");
    assert_eq!(
        reduce(mode, &DetailAction::Cancel, &row, &ctx),
        DetailMode::Viewing
    );
}

#[test]
fn typing_clears_the_error_and_a_form_shows_the_one_it_was_given() {
    let row = plain();
    let ctx = ctx();
    let mode = reduce(
        DetailMode::Viewing,
        &DetailAction::BeginEdit(EditField::Fix),
        &row,
        &ctx,
    )
    .with_error("The server said no.".into());
    assert_eq!(mode.error(), Some("The server said no."));
    let mode = reduce(mode, &DetailAction::Text("x".into()), &row, &ctx);
    assert_eq!(mode.error(), None);
    assert_eq!(DetailMode::Viewing.error(), None);
    assert!(!DetailMode::Viewing.is_form() && !DetailMode::PickField.is_form());
    assert_eq!(
        commit(&DetailMode::Viewing, &row, &ctx, "r"),
        Err("Nothing is open to commit.".to_string())
    );
}

#[test]
fn the_undo_bar_lasts_five_seconds_and_says_what_it_undoes() {
    let edit = DetailUndo::new(
        UndoAction::DiscardEdit("edit-1".into()),
        "Headline edited.".into(),
        1_000,
    );
    assert_eq!(edit.button(), "Undo edit");
    assert_eq!(edit.seconds_left(1_000), Some(5));
    assert_eq!(edit.seconds_left(3_200), Some(3));
    assert_eq!(
        edit.seconds_left(6_000),
        None,
        "closed at exactly five seconds"
    );
    let merge = DetailUndo::new(UndoAction::RevokeDecision("dec-1".into()), "m".into(), 0);
    assert_eq!(merge.button(), "Undo");
}

#[test]
fn applied_edits_are_listed_by_field() {
    let edited = rows()
        .into_iter()
        .find(|r| r.edits.iter().any(|e| !e.stale))
        .unwrap();
    let by = applied_by_field(&edited);
    assert!(!by.is_empty());
    for e in edited.edits.iter().filter(|e| !e.stale) {
        assert!(by.contains_key(e.field.wire_name()));
    }
}

// ── The bridge to the code viewer and the stacked view ───────────────────────────────────

#[test]
fn a_site_is_the_finding_the_viewer_reads_under_the_same_key() {
    use crate::triage::finding_key;
    let all = rows();
    for row in all.iter().take(60) {
        let view = bridge::finding_view_of(row);
        assert_eq!(finding_key(&view), row.site_key, "one key for both");
        assert_eq!(view.path, row.path);
        assert_eq!(view.line, row.line);
        assert_eq!(view.rule_id, row.rule_id);
        assert_eq!(view.snippet, row.snippet);
        assert!(
            !view.detail.contains('\u{2014}'),
            "no dash in rendered text"
        );
        assert!(view.detail.contains(&row.headline));
    }
    let views = bridge::finding_views(all.iter().take(5));
    assert_eq!(views.len(), 5);
}

#[test]
fn the_viewers_dispositions_are_the_sites_decisions() {
    use crate::triage::TriageState;
    let all = rows();
    let map = bridge::dispositions_of(all.iter());
    for row in &all {
        match (&row.decision, map.get(&row.site_key)) {
            (Some(d), Some(disp)) => {
                assert_eq!(disp.state, bridge::triage_state_of(&d.state));
                assert_eq!(disp.reason, d.reason);
            }
            (Some(d), None) => assert_eq!(
                bridge::triage_state_of(&d.state),
                TriageState::Unresolved,
                "only a restore is left out"
            ),
            (None, found) => assert!(found.is_none()),
        }
    }
    assert_eq!(
        bridge::triage_state_of(&DecisionState::TechDebtNow),
        TriageState::TechDebt
    );
    assert_eq!(
        bridge::triage_state_of(&DecisionState::MergedInto("k".into())),
        TriageState::MergedInto
    );
}

// ── The context a lane gives the detail ───────────────────────────────────────────────────

#[test]
fn the_context_names_the_crumbs_the_rules_the_other_sub_groups_and_the_loaded_sites() {
    use crate::tree::state::{TreeIndex, TreeState};
    use camerata_api_types::review_view::TreeLevel;
    let ds = dataset_base();
    let index = TreeIndex::new(&ds.tree);
    // A sub-group with a sibling, opened with its first page.
    let sub = ds
        .tree
        .nodes
        .iter()
        .find(|n| {
            n.level == TreeLevel::Subgroup
                && !n.thrown_out
                && n.parent.as_ref().is_some_and(|p| {
                    index
                        .children(p)
                        .filter(|c| c.level == TreeLevel::Subgroup)
                        .count()
                        > 1
                })
        })
        .expect("a sub-group with a sibling");
    let mut state = TreeState::default();
    for r in state.open_path(&index, &sub.id) {
        state.apply_page(
            &ds.sites_page(&r.node, r.cursor.as_deref(), r.limit)
                .unwrap(),
        );
    }
    let mut row = state.loaded[&sub.id].rows[0].clone();
    row.new_since_decision = None;
    let ctx = context_for(&index, &state, &sub.id, &row);

    assert_eq!(ctx.crumbs.len(), 3, "category, rule, sub-group");
    assert_eq!(ctx.crumbs[2], sub.label);
    let rule_node = sub.parent.clone().unwrap();
    assert!(
        ctx.rule_ids
            .contains(&rule_node.trim_start_matches("rule:").to_string()),
        "the run's rules are the rule select's"
    );
    assert!(!ctx.subgroups.is_empty());
    assert!(
        !ctx.subgroups.contains(&sub.label),
        "a split offers the other sub-groups"
    );
    assert_eq!(ctx.pool.len(), state.loaded[&sub.id].rows.len());
    assert!(ctx.pool.iter().any(|p| p.key == row.site_key));
    assert_eq!(ctx.node_decision, None);
    // The tree's merge picker keeps its own remedy for an empty match.
    assert_eq!(
        ctx.pool_none,
        "No loaded site matches. Open its sub-group in the tree first."
    );

    // A row new since a group decision learns the group decision's state.
    let decided = ds
        .tree
        .nodes
        .iter()
        .find(|n| n.decision.is_some())
        .expect("a decided node");
    let mut new_row = row.clone();
    new_row.new_since_decision = Some(NewSinceDecisionView {
        decision_id: "dec-1".into(),
        node_id: decided.id.clone(),
        decided_at: "2026-11-04T09:00:00Z".into(),
        covered: 3,
        node_state: None,
    });
    let ctx = context_for(&index, &state, &sub.id, &new_row);
    assert_eq!(
        ctx.node_decision,
        decided.decision.as_ref().map(|d| d.state.clone())
    );
    // A thrown-out rule is not offered, and the select keeps each rule once.
    let mut seen = ctx.rule_ids.clone();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), ctx.rule_ids.len());
}

#[test]
fn the_head_chips_carry_the_escalation_reason_and_rank_and_name_the_outlier() {
    let all = rows();
    let esc = all
        .iter()
        .find(|r| {
            r.markers.contains(&Marker::Escalated)
                && r.stage2
                    .as_ref()
                    .is_some_and(|s| s.reason.is_some() && s.global_rank.is_some())
        })
        .expect("an escalated row");
    let s2 = esc.stage2.as_ref().unwrap();
    let chip = head_chips(esc)
        .into_iter()
        .find(|c| c.class.starts_with("mark-esc"))
        .unwrap();
    assert!(chip.label.starts_with("ESC"), "{}", chip.label);
    assert!(
        chip.label.contains(s2.reason.unwrap().phrase()),
        "{}",
        chip.label
    );
    assert!(
        chip.label
            .ends_with(&format!("#{}", s2.global_rank.unwrap())),
        "{}",
        chip.label
    );
    // The tree's own chip stays the short one.
    let short = crate::tree::marker_chips(esc)
        .into_iter()
        .find(|c| c.class.starts_with("mark-esc"))
        .unwrap();
    assert!(short.label == "ESC" || short.label == "ESC over");

    let outlier = all
        .iter()
        .find(|r| r.markers.contains(&Marker::Outlier))
        .unwrap();
    let chip = head_chips(outlier)
        .into_iter()
        .find(|c| c.class == "mark-outlier")
        .unwrap();
    assert!(chip.label.starts_with("outlier: "), "{}", chip.label);

    // An added finding carries the edit mark whether or not the server marked it.
    let mut added = plain();
    added.added_by_reviewer = true;
    assert!(head_chips(&added).iter().any(|c| c.class == "mark-edited"));
    added.markers.push(Marker::Edited);
    assert_eq!(
        head_chips(&added)
            .iter()
            .filter(|c| c.class == "mark-edited")
            .count(),
        1,
        "never twice"
    );
}

#[test]
fn the_split_select_values_map_back_to_the_choice_and_never_carry_a_label() {
    let subs: Vec<String> = vec!["sub 1: policies".into(), "a\u{0}b".into()];
    // "New sub-group" has one value, and the same constant maps back to New.
    assert_eq!(
        split_option_value(&subs, &SplitChoice::New),
        SPLIT_NEW_VALUE
    );
    assert_eq!(
        split_choice_from_value(&subs, SPLIT_NEW_VALUE),
        Some(SplitChoice::New)
    );
    // An existing sub-group is its position, whatever its label holds.
    let second = SplitChoice::Existing("a\u{0}b".into());
    let value = split_option_value(&subs, &second);
    assert_eq!(value, "1");
    assert!(!value.contains('\u{0}'));
    assert_eq!(
        split_choice_from_value(&subs, "0"),
        Some(SplitChoice::Existing("sub 1: policies".into()))
    );
    assert_eq!(split_choice_from_value(&subs, &value), Some(second));
    // A value the form never offered is nothing, not a sub-group named after it.
    assert_eq!(split_choice_from_value(&subs, "7"), None);
    assert_eq!(split_choice_from_value(&subs, "sub 1: policies"), None);
    // A choice naming a label the rule no longer has falls back to the new-sub-group value.
    assert_eq!(
        split_option_value(&subs, &SplitChoice::Existing("gone".into())),
        SPLIT_NEW_VALUE
    );
}

#[test]
fn the_merge_picker_reports_every_match_and_says_when_it_shows_fewer() {
    let row = plain();
    let pool: Vec<PoolSite> = (0..20)
        .map(|i| PoolSite {
            key: format!("k{i}"),
            path: format!("src/m{i}.ts"),
            line: i + 1,
            rule_id: "SEC-RLS-1".into(),
            merged: false,
        })
        .collect();
    let ctx = DetailContext { pool, ..ctx() };
    let list = merge_candidates(&row, &ctx, "");
    assert_eq!(list.len(), MERGE_LIST_MAX, "the list is capped");
    assert_eq!(
        merge_candidate_total(&row, &ctx, ""),
        20,
        "the total is every match, not the page"
    );
    // A search that fits shows all of it and says nothing.
    assert_eq!(merge_candidate_total(&row, &ctx, "m1"), 11);
    assert_eq!(merge_shown_line(11, 11), None);
    assert_eq!(
        merge_shown_line(12, 20).as_deref(),
        Some("12 of 20 shown, narrow the search")
    );
}

#[test]
fn the_category_select_offers_exactly_the_engines_taxonomy() {
    let mut row = plain();
    row.category = None;
    let Control::Select(options) = control_for(EditField::Category, &row, &ctx()) else {
        panic!("the category is a select");
    };
    let offered: Vec<&str> = options
        .iter()
        .map(|o| o.value.as_str())
        .filter(|v| !v.is_empty())
        .collect();
    assert_eq!(offered, camerata_api_types::edits::KNOWN_CATEGORIES);
}

// ── A row the engine threw out (wave 19 D1) ──────────────────────────────────────────────

fn thrown_out() -> SiteRow {
    let mut row = plain();
    row.thrown_out_reason = Some("its headline is a placeholder".to_string());
    row
}

#[test]
fn a_thrown_out_row_refuses_every_ruling_but_a_false_positive() {
    let row = thrown_out();
    assert!(is_thrown_out(&row));
    for st in [
        TriageState::Confirmed,
        TriageState::Ignored,
        TriageState::TechDebt,
    ] {
        assert!(ruling_refused(&row, st), "{st:?}");
        let err = ruling_request(&row, st, "a reason", "run-1").unwrap_err();
        assert_eq!(err, THROWN_OUT_REFUSAL);
    }
    // The false positive is the confirmation: it is posted.
    assert!(!ruling_refused(&row, TriageState::FalsePositive));
    let req = ruling_request(&row, TriageState::FalsePositive, " rightly out ", "run-1").unwrap();
    assert_eq!(req.state, DecisionState::FalsePositive);
    assert_eq!(req.key, row.site_key);
    assert_eq!(req.reason, "rightly out");
    // A kept row takes every ruling.
    let kept = plain();
    assert!(!is_thrown_out(&kept));
    for st in [
        TriageState::Confirmed,
        TriageState::Ignored,
        TriageState::TechDebt,
        TriageState::FalsePositive,
    ] {
        assert!(!ruling_refused(&kept, st), "{st:?}");
        assert!(ruling_request(&kept, st, "r", "run-1").is_ok());
    }
}

#[test]
fn a_thrown_out_row_opens_only_the_put_back_and_confirm_forms() {
    let row = thrown_out();
    // Merge into is a ruling the server refuses on it: the form does not open.
    let m = reduce(DetailMode::Viewing, &DetailAction::BeginMerge, &row, &ctx());
    assert_eq!(m, DetailMode::Viewing);
    for choice in [ThrowOutChoice::PutBack, ThrowOutChoice::Confirm] {
        let open = reduce(
            DetailMode::Viewing,
            &DetailAction::BeginThrowOut(choice),
            &row,
            &ctx(),
        );
        assert!(
            matches!(&open, DetailMode::ThrowOut(d) if d.choice == choice),
            "{open:?}"
        );
        assert!(open.is_form());
        let typed = reduce(open, &DetailAction::Reason("because".into()), &row, &ctx());
        assert!(matches!(&typed, DetailMode::ThrowOut(d) if d.reason == "because"));
    }
    // A kept row has no such form, and keeps its merge.
    let kept = plain();
    assert_eq!(
        reduce(
            DetailMode::Viewing,
            &DetailAction::BeginThrowOut(ThrowOutChoice::PutBack),
            &kept,
            &ctx()
        ),
        DetailMode::Viewing
    );
    assert!(matches!(
        reduce(
            DetailMode::Viewing,
            &DetailAction::BeginMerge,
            &kept,
            &ctx()
        ),
        DetailMode::Merge(_)
    ));
}

#[test]
fn putting_back_or_confirming_needs_a_reason_and_posts_the_one_decision_each_means() {
    let row = thrown_out();
    for (choice, state) in [
        (ThrowOutChoice::PutBack, DecisionState::EngineRestored),
        (ThrowOutChoice::Confirm, DecisionState::FalsePositive),
    ] {
        let open = reduce(
            DetailMode::Viewing,
            &DetailAction::BeginThrowOut(choice),
            &row,
            &ctx(),
        );
        let err = commit(&open, &row, &ctx(), "run-1").unwrap_err();
        assert!(err.contains("reason"), "{err}");
        let typed = reduce(
            open,
            &DetailAction::Reason("a real problem".into()),
            &row,
            &ctx(),
        );
        let DetailWrite::Decision { req, done } = commit(&typed, &row, &ctx(), "run-1").unwrap()
        else {
            panic!("a decision");
        };
        assert_eq!(req.state, state);
        assert_eq!(req.key, row.site_key);
        assert_eq!(req.reason, "a real problem");
        assert!(!done.is_empty());
    }
}

#[test]
fn a_stale_decision_on_a_thrown_out_row_offers_reapply_only_when_the_server_would_take_it() {
    let mut row = rows()
        .into_iter()
        .find(|r| r.stale_decision.is_some() && r.decision.is_none())
        .expect("a stale decision row");
    row.thrown_out_reason = Some("a placeholder".to_string());
    for (state, actions) in [
        (DecisionState::Confirmed, vec![LineAction::Discard]),
        (DecisionState::Ignored, vec![LineAction::Discard]),
        (
            DecisionState::FalsePositive,
            vec![LineAction::Reapply, LineAction::Discard],
        ),
    ] {
        row.stale_decision.as_mut().unwrap().state = state.clone();
        assert_eq!(decision_line(&row, &ctx()).actions, actions, "{state:?}");
    }
}

/// Wave 20 D1 residue: the edits route searches only the report's findings, so an edit on a
/// thrown-out row answers `unknown_site`. The form (and the E chord) does not open on one.
#[test]
fn a_thrown_out_row_opens_no_edit_form() {
    let row = thrown_out();
    for action in [
        DetailAction::PickField,
        DetailAction::BeginEdit(EditField::Headline),
        DetailAction::BeginEdit(EditField::Severity),
        DetailAction::BeginEdit(EditField::Subgroup),
        DetailAction::BeginSplit,
    ] {
        let next = reduce(DetailMode::Viewing, &action, &row, &ctx());
        assert_eq!(next, DetailMode::Viewing, "{action:?}");
    }
    // A kept row keeps them.
    let kept = plain();
    assert!(matches!(
        reduce(
            DetailMode::Viewing,
            &DetailAction::BeginEdit(EditField::Headline),
            &kept,
            &ctx()
        ),
        DetailMode::Edit(_)
    ));
    assert_eq!(
        reduce(DetailMode::Viewing, &DetailAction::PickField, &kept, &ctx()),
        DetailMode::PickField
    );
}
