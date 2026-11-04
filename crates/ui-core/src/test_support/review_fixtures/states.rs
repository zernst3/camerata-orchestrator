//! One fixture per named gallery state (review UI spec section 12), by id.

use std::collections::BTreeMap;

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::edits::EditField;
use camerata_api_types::review::EscalationBudget;
use camerata_api_types::review_view::{
    EscalationBudgetOutcome, EscalationBudgetReq, Marker, SiteRow, TreeLevel, SITES_PAGE_SIZE,
};

use super::dataset::{build, EngineMode, Params, ReviewDataSet};
use super::{
    dataset_30k, dataset_base, Body, DetailBody, DetailForm, EngineBody, ExportBody, Loaded,
    QueueBody, SettingsBody, StateFixture, TreeBody, SEED,
};

/// Every id this module can build, in the order of the spec's section 12.
pub const FIXTURE_IDS: [&str; 56] = [
    "shell/empty",
    "shell/loading",
    "shell/error",
    "shell/held-count",
    "tree/empty",
    "tree/loading",
    "tree/error",
    "tree/30k",
    "tree/markers",
    "tree/node-prompt",
    "tree/node-decided",
    "tree/not-group-reviewed",
    "tree/narrow",
    "queue/empty",
    "queue/loading",
    "queue/error",
    "queue/at-budget",
    "queue/over-budget",
    "queue/line-lowered",
    "queue/rate-changed-toast",
    "queue/30k",
    "engine/lane-off",
    "engine/loading",
    "engine/card-pending",
    "engine/no-list-open",
    "engine/no-pending-correction",
    "engine/no-corrected",
    "engine/all-verdicts",
    "engine/small-category",
    "detail/empty",
    "detail/plain",
    "detail/escalated",
    "detail/outlier",
    "detail/edited-field",
    "detail/stale-edit",
    "detail/stale-decision",
    "detail/moved",
    "detail/new-since-decision",
    "detail/held",
    "detail/merged",
    "detail/added",
    "detail/ignored-needs-ack",
    "detail/ignored-acked",
    "detail/edit-open",
    "detail/merge-picker",
    "detail/split-picker",
    "detail/add-form",
    "detail/add-refused",
    "settings/defaults",
    "settings/rate-changed",
    "settings/invalid",
    "settings/saving",
    "settings/error",
    "settings/run-in-flight",
    "export/clear",
    "export/blocked",
];

fn params(f: impl FnOnce(&mut Params)) -> Params {
    let mut p = Params::base(SEED);
    f(&mut p);
    p
}

/// A run with nothing in it.
fn empty_params() -> Params {
    params(|p| {
        p.sites = 0;
        p.thrown_out = 0;
        p.candidates = 0;
        p.outliers = 0;
        p.held = 0;
        p.merged_without_decision = 0;
        p.edited = 0;
        p.stale_edits = 0;
        p.stale_decisions = 0;
        p.moved = 0;
        p.duplicates = 0;
        p.decided_subgroups = 0;
        p.fully_decided_subgroups = 0;
        p.decided_rules = 0;
        p.decision_gaps = 0;
        p.new_since_decision = 0;
        p.site_decisions = 0;
        p.added = 0;
        p.counted_lines = 0;
    })
}

fn shell_of(ds: &ReviewDataSet) -> Loaded<camerata_api_types::review_view::ReviewShellView> {
    Loaded::Ready(ds.shell.clone())
}

fn first_row(ds: &ReviewDataSet, pred: impl Fn(&SiteRow) -> bool) -> Option<SiteRow> {
    ds.rows.iter().find(|r| pred(r)).cloned()
}

/// A row with no marker and no decision, read by stage 1.
fn is_plain(r: &SiteRow) -> bool {
    r.markers.is_empty()
        && r.decision.is_none()
        && r.thrown_out_reason.is_none()
        && !r.stage1.real.is_empty()
        && r.stage2
            .as_ref()
            .is_some_and(|s| s.outlier_reason.is_none())
}

fn ancestors(ds: &ReviewDataSet, node: &str) -> Vec<String> {
    let mut chain = vec![node.to_string()];
    let mut at = node.to_string();
    while let Some(parent) = ds
        .tree
        .nodes
        .iter()
        .find(|n| n.id == at)
        .and_then(|n| n.parent.clone())
    {
        chain.insert(0, parent.clone());
        at = parent;
    }
    chain
}

/// Open each sub-group in `subs`: its ancestors and its first page of sites.
fn open_subs(
    ds: &ReviewDataSet,
    subs: &[String],
) -> (
    Vec<String>,
    BTreeMap<String, camerata_api_types::review_view::SitesPage>,
) {
    let mut open: Vec<String> = Vec::new();
    let mut pages = BTreeMap::new();
    for sub in subs {
        for id in ancestors(ds, sub) {
            if !open.contains(&id) {
                open.push(id);
            }
        }
        if let Some(page) = ds.sites_page(sub, None, SITES_PAGE_SIZE) {
            pages.insert(sub.clone(), page);
        }
    }
    (open, pages)
}

fn subgroup_ids(ds: &ReviewDataSet) -> Vec<String> {
    ds.tree
        .nodes
        .iter()
        .filter(|n| n.level == TreeLevel::Subgroup && !n.thrown_out)
        .map(|n| n.id.clone())
        .collect()
}

/// The markers on the first page of `sub`.
fn markers_on_first_page(ds: &ReviewDataSet, sub: &str) -> (Vec<Marker>, usize) {
    let Some(page) = ds.sites_page(sub, None, SITES_PAGE_SIZE) else {
        return (Vec::new(), 0);
    };
    let found = Marker::PRECEDENCE
        .into_iter()
        .filter(|m| page.rows.iter().any(|r| r.markers.contains(m)))
        .collect();
    (found, page.rows.len())
}

/// Sub-groups that together show every marker on their first page. The first is the richest
/// sub-group (most distinct markers, then fewest rows), which the page focuses; the rest are
/// picked by markers gained per row, so the page stays short and the focused node, drawn in tree
/// order, is not pushed below the fold by long sub-groups opened only for one chip.
fn subs_covering_every_marker(ds: &ReviewDataSet) -> Vec<String> {
    let mut remaining: Vec<Marker> = Marker::PRECEDENCE.to_vec();
    let mut candidates = subgroup_ids(ds);
    let info: BTreeMap<String, (Vec<Marker>, usize)> = candidates
        .iter()
        .map(|s| (s.clone(), markers_on_first_page(ds, s)))
        .collect();
    let mut chosen: Vec<String> = Vec::new();
    while !remaining.is_empty() && chosen.len() < 8 {
        let gain = |sub: &String| -> usize {
            info[sub].0.iter().filter(|m| remaining.contains(m)).count()
        };
        let best = if chosen.is_empty() {
            candidates.iter().max_by_key(|s| {
                (
                    info[*s].0.len(),
                    std::cmp::Reverse(info[*s].1),
                    std::cmp::Reverse((*s).clone()),
                )
            })
        } else {
            // The rarest marker still missing decides which sub-groups are in the running (a
            // marker only one sub-group has must come from it); among those, the most markers
            // gained per row, compared as a fraction without dividing.
            let rarest = remaining
                .iter()
                .min_by_key(|m| info.values().filter(|(ms, _)| ms.contains(m)).count())
                .copied();
            candidates
                .iter()
                .filter(|s| rarest.is_some_and(|m| info[*s].0.contains(&m)))
                .max_by(|a, b| {
                    let (ga, ra) = (gain(a), info[*a].1.max(1));
                    let (gb, rb) = (gain(b), info[*b].1.max(1));
                    (ga * rb).cmp(&(gb * ra)).then_with(|| b.cmp(a))
                })
        };
        let Some(best) = best.cloned() else {
            break;
        };
        if gain(&best) == 0 {
            break;
        }
        remaining.retain(|m| !info[&best].0.contains(m));
        candidates.retain(|c| *c != best);
        chosen.push(best);
    }
    chosen
}

fn tree_body(ds: &ReviewDataSet, subs: &[String]) -> TreeBody {
    let (open_nodes, pages) = open_subs(ds, subs);
    TreeBody {
        tree: Loaded::Ready(ds.tree.clone()),
        open_nodes,
        pages,
        selected_site: None,
        prompt_node: None,
        narrow: false,
    }
}

fn detail(ds: &ReviewDataSet, site: Option<SiteRow>, form: Option<DetailForm>) -> StateFixture {
    let siblings = site
        .as_ref()
        .and_then(|s| ds.node_rows.get(&s.group_path[2]))
        .map(|members| {
            members
                .iter()
                .map(|&i| &ds.rows[i])
                .filter(|r| Some(&r.site_key) != site.as_ref().map(|s| &s.site_key))
                .take(8)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    StateFixture {
        shell: shell_of(ds),
        body: Body::Detail(Box::new(DetailBody {
            site,
            form,
            siblings,
        })),
    }
}

fn settings_body(ds: &ReviewDataSet) -> SettingsBody {
    SettingsBody {
        view: Loaded::Ready(ds.settings.clone()),
        draft: None,
        preview: None,
        saving: false,
        save_error: None,
        run_in_flight: false,
    }
}

/// The formula changed to `rate`: the data set it would give, the draft and the preview.
fn rate_change(
    base: &ReviewDataSet,
    rate: f32,
) -> (ReviewDataSet, EscalationBudgetReq, EscalationBudgetOutcome) {
    let changed = build(&params(|p| p.rate_per_kloc = rate));
    let before: EscalationBudget = base.shell.budget;
    let after: EscalationBudget = changed.shell.budget;
    let outcome = EscalationBudgetOutcome {
        applied: true,
        before,
        after,
        moved_above: after.escalated.saturating_sub(before.escalated),
        moved_below: before.escalated.saturating_sub(after.escalated),
    };
    let req = EscalationBudgetReq {
        rate_per_kloc: rate,
        floor: changed.settings.escalation.floor,
        over_budget_reasons: changed.settings.escalation.over_budget_reasons.clone(),
    };
    (changed, req, outcome)
}

fn engine_body(ds: &ReviewDataSet, selected: Option<usize>, wrong_list_open: bool) -> StateFixture {
    StateFixture {
        shell: shell_of(ds),
        body: Body::Engine(EngineBody {
            lane: Loaded::Ready(ds.engine.clone()),
            selected_card: selected,
            wrong_list_open,
        }),
    }
}

fn card_with_wrong_verdict(ds: &ReviewDataSet) -> Option<usize> {
    ds.engine
        .cards
        .iter()
        .position(|c| c.verdict.as_ref().is_some_and(|v| !v.right))
}

/// The fixture for a named state, or `None` for a name that is not one.
pub fn fixture(id: &str) -> Option<StateFixture> {
    let base = || dataset_base();
    Some(match id {
        // ── shell ──
        "shell/empty" => {
            let ds = build(&empty_params());
            StateFixture {
                shell: shell_of(&ds),
                body: Body::ShellOnly,
            }
        }
        "shell/loading" => StateFixture {
            shell: Loaded::Loading,
            body: Body::ShellOnly,
        },
        "shell/error" => StateFixture {
            shell: Loaded::Failed(
                "The review service did not answer at 127.0.0.1:8787. Check that it is running."
                    .to_string(),
            ),
            body: Body::ShellOnly,
        },
        "shell/held-count" => {
            let ds = base();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::ShellOnly,
            }
        }

        // ── tree ──
        "tree/empty" => {
            let ds = build(&empty_params());
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(tree_body(&ds, &[])),
            }
        }
        "tree/loading" => {
            let ds = base();
            let mut body = tree_body(&ds, &[]);
            body.tree = Loaded::Loading;
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(body),
            }
        }
        "tree/error" => {
            let ds = base();
            let mut body = tree_body(&ds, &[]);
            body.tree = Loaded::Failed("The tree request failed with status 500.".to_string());
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(body),
            }
        }
        "tree/30k" => {
            let ds = dataset_30k();
            let biggest = subgroup_ids(&ds)
                .into_iter()
                .max_by_key(|s| {
                    (
                        ds.node_rows.get(s).map_or(0, Vec::len),
                        std::cmp::Reverse(s.clone()),
                    )
                })
                .into_iter()
                .collect::<Vec<_>>();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(tree_body(&ds, &biggest)),
            }
        }
        "tree/markers" => {
            let ds = base();
            let subs = subs_covering_every_marker(&ds);
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(tree_body(&ds, &subs)),
            }
        }
        "tree/node-prompt" => {
            let ds = base();
            // A sub-group with an outlier and a held row, so the prompt has something to exclude.
            let sub = subgroup_ids(&ds)
                .into_iter()
                .filter(|s| {
                    ds.tree
                        .nodes
                        .iter()
                        .any(|n| n.id == *s && n.decision.is_none())
                })
                .max_by_key(|s| {
                    let n = ds.tree.nodes.iter().find(|n| n.id == *s).map(|n| n.counts);
                    n.map_or((0, 0, 0), |c| {
                        (
                            usize::from(c.outliers > 0 && c.held > 0),
                            c.outliers + c.held,
                            c.sites,
                        )
                    })
                })?;
            let mut body = tree_body(&ds, std::slice::from_ref(&sub));
            body.prompt_node = Some(sub);
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(body),
            }
        }
        "tree/node-decided" => {
            let ds = base();
            let sub = subgroup_ids(&ds)
                .into_iter()
                .filter(|s| {
                    ds.tree
                        .nodes
                        .iter()
                        .any(|n| n.id == *s && n.decision.is_some())
                })
                .max_by_key(|s| {
                    ds.tree
                        .nodes
                        .iter()
                        .find(|n| n.id == *s)
                        .map_or(0, |n| n.counts.new_since_decision)
                })?;
            // A sub-group every site of which is decided, so its green check is on the page.
            let whole = ds
                .tree
                .nodes
                .iter()
                .find(|n| {
                    n.level == TreeLevel::Subgroup
                        && n.id != sub
                        && n.decision.is_some()
                        && n.counts.sites > 0
                        && n.counts.decided == n.counts.sites
                })
                .map(|n| n.id.clone());
            let subs: Vec<String> = std::iter::once(sub).chain(whole).collect();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(tree_body(&ds, &subs)),
            }
        }
        "tree/not-group-reviewed" => {
            let ds = build(&params(|p| p.group_reviewed = false));
            let sub = subgroup_ids(&ds).into_iter().next();
            let subs: Vec<String> = sub.into_iter().collect();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(tree_body(&ds, &subs)),
            }
        }
        "tree/narrow" => {
            let ds = base();
            let subs = subs_covering_every_marker(&ds);
            let mut body = tree_body(&ds, &subs);
            body.narrow = true;
            body.selected_site = subs
                .first()
                .and_then(|s| body.pages.get(s))
                .and_then(|p| p.rows.first())
                .map(|r| r.site_key.clone());
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Tree(body),
            }
        }

        // ── queue ──
        "queue/empty" => {
            let ds = build(&empty_params());
            queue_state(&ds, ds.queue(false, 0, 100))
        }
        "queue/loading" => {
            let ds = base();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Queue(QueueBody {
                    queue: Loaded::Loading,
                    selected_site: None,
                    rate_toast: None,
                }),
            }
        }
        "queue/error" => {
            let ds = base();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Queue(QueueBody {
                    queue: Loaded::Failed("The queue request failed with status 500.".to_string()),
                    selected_site: None,
                    rate_toast: None,
                }),
            }
        }
        "queue/at-budget" => {
            let ds = build(&params(|p| p.over_budget_reasons.clear()));
            queue_state(&ds, ds.queue(false, 0, 100))
        }
        "queue/over-budget" => {
            let ds = base();
            queue_state(&ds, ds.queue(false, 0, 100))
        }
        "queue/line-lowered" => {
            let ds = base();
            queue_state(&ds, ds.queue(true, 0, 100))
        }
        "queue/rate-changed-toast" => {
            let ds = base();
            let (changed, _, outcome) = rate_change(&ds, 2.0);
            let mut fx = queue_state(&changed, changed.queue(false, 0, 100));
            if let Body::Queue(q) = &mut fx.body {
                q.rate_toast = Some(outcome);
            }
            fx
        }
        "queue/30k" => {
            let ds = dataset_30k();
            queue_state(&ds, ds.queue(false, 0, 100))
        }

        // ── engine ──
        "engine/lane-off" => {
            let ds = build(&params(|p| p.sample_size = 0));
            engine_body(&ds, None, false)
        }
        "engine/loading" => {
            let ds = base();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Engine(EngineBody {
                    lane: Loaded::Loading,
                    selected_card: None,
                    wrong_list_open: false,
                }),
            }
        }
        "engine/card-pending" => engine_body(&base(), Some(0), false),
        "engine/no-list-open" => engine_body(&base(), Some(0), true),
        "engine/no-pending-correction" => {
            let ds = build(&params(|p| p.engine_mode = EngineMode::NoWithoutCorrection));
            let at = card_with_wrong_verdict(&ds);
            engine_body(&ds, at, false)
        }
        "engine/no-corrected" => {
            let ds = build(&params(|p| p.engine_mode = EngineMode::NoCorrected));
            let at = card_with_wrong_verdict(&ds);
            engine_body(&ds, at, false)
        }
        "engine/all-verdicts" => {
            let ds = build(&params(|p| p.engine_mode = EngineMode::AllVerdicts));
            engine_body(&ds, Some(0), false)
        }
        "engine/small-category" => {
            let ds = build(&params(|p| {
                p.sample_size = 24;
                p.engine_mode = EngineMode::AllVerdicts;
            }));
            engine_body(&ds, Some(0), false)
        }

        // ── detail ──
        "detail/empty" => detail(&base(), None, None),
        "detail/plain" => {
            let ds = base();
            detail(&ds, first_row(&ds, is_plain), None)
        }
        "detail/escalated" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| {
                    r.markers.contains(&Marker::Escalated)
                        && !r.markers.contains(&Marker::OverBudget)
                        && r.decision.is_none()
                }),
                None,
            )
        }
        "detail/outlier" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| r.markers.contains(&Marker::Outlier)),
                None,
            )
        }
        "detail/edited-field" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| r.markers.contains(&Marker::Edited)),
                None,
            )
        }
        "detail/stale-edit" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| r.edits.iter().any(|e| e.stale)),
                None,
            )
        }
        "detail/stale-decision" => {
            let ds = base();
            detail(&ds, first_row(&ds, |r| r.stale_decision.is_some()), None)
        }
        "detail/moved" => {
            let ds = base();
            detail(&ds, first_row(&ds, |r| r.moved_from.is_some()), None)
        }
        "detail/new-since-decision" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| r.new_since_decision.is_some()),
                None,
            )
        }
        "detail/held" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| {
                    r.held == Some(camerata_api_types::review_view::HoldReason::EvidenceUnverified)
                }),
                None,
            )
        }
        "detail/merged" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| {
                    r.decision
                        .as_ref()
                        .is_some_and(|d| matches!(d.state, DecisionState::MergedInto(_)))
                }),
                None,
            )
        }
        "detail/added" => {
            let ds = base();
            detail(&ds, first_row(&ds, |r| r.added_by_reviewer), None)
        }
        "detail/ignored-needs-ack" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| {
                    r.decision.as_ref().is_some_and(|d| {
                        d.state == DecisionState::Ignored && d.confirmed_by_client.is_none()
                    })
                }),
                None,
            )
        }
        "detail/ignored-acked" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, |r| {
                    r.decision.as_ref().is_some_and(|d| {
                        d.state == DecisionState::Ignored && d.confirmed_by_client.is_some()
                    })
                }),
                None,
            )
        }
        "detail/edit-open" => {
            let ds = base();
            detail(
                &ds,
                first_row(&ds, is_plain),
                Some(DetailForm::Edit(EditField::Fix)),
            )
        }
        "detail/merge-picker" => {
            let ds = base();
            detail(&ds, first_row(&ds, is_plain), Some(DetailForm::MergePicker))
        }
        "detail/split-picker" => {
            let ds = base();
            detail(&ds, first_row(&ds, is_plain), Some(DetailForm::SplitPicker))
        }
        "detail/add-form" => {
            let ds = base();
            detail(&ds, first_row(&ds, is_plain), Some(DetailForm::AddForm))
        }
        "detail/add-refused" => {
            let ds = base();
            let site = first_row(&ds, is_plain);
            let msg = site.as_ref().map_or_else(String::new, |s| {
                format!(
                    "A finding for {} already exists at {}:{}. Edit that one instead.",
                    s.rule_id, s.path, s.line
                )
            });
            detail(&ds, site, Some(DetailForm::AddRefused(msg)))
        }

        // ── settings ──
        "settings/defaults" => {
            let ds = base();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Settings(settings_body(&ds)),
            }
        }
        "settings/rate-changed" => {
            let ds = base();
            let (_, req, outcome) = rate_change(&ds, 2.0);
            let mut body = settings_body(&ds);
            body.draft = Some(req);
            body.preview = Some(outcome);
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Settings(body),
            }
        }
        "settings/invalid" => {
            let ds = base();
            let mut body = settings_body(&ds);
            body.draft = Some(EscalationBudgetReq {
                rate_per_kloc: ds.settings.limits.rate_max + 2.5,
                floor: ds.settings.escalation.floor,
                over_budget_reasons: ds.settings.escalation.over_budget_reasons.clone(),
            });
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Settings(body),
            }
        }
        "settings/saving" => {
            let ds = base();
            let (_, req, outcome) = rate_change(&ds, 2.0);
            let mut body = settings_body(&ds);
            body.draft = Some(req);
            body.preview = Some(outcome);
            body.saving = true;
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Settings(body),
            }
        }
        "settings/error" => {
            let ds = base();
            let (_, req, outcome) = rate_change(&ds, 2.0);
            let mut body = settings_body(&ds);
            body.draft = Some(req);
            body.preview = Some(outcome);
            body.save_error =
                Some("The setting was not saved: the project file is read only.".to_string());
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Settings(body),
            }
        }
        "settings/run-in-flight" => {
            let ds = base();
            let mut body = settings_body(&ds);
            body.run_in_flight = true;
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Settings(body),
            }
        }

        // ── export ──
        "export/clear" => {
            let ds = build(&params(|p| {
                p.held = 0;
                p.merged_without_decision = 0;
                p.decision_gaps = 0;
            }));
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Export(ExportBody {
                    gate: ds.gate.clone(),
                }),
            }
        }
        "export/blocked" => {
            let ds = base();
            StateFixture {
                shell: shell_of(&ds),
                body: Body::Export(ExportBody {
                    gate: ds.gate.clone(),
                }),
            }
        }
        _ => return None,
    })
}

fn queue_state(
    ds: &ReviewDataSet,
    queue: camerata_api_types::review_view::QueueView,
) -> StateFixture {
    StateFixture {
        shell: shell_of(ds),
        body: Body::Queue(QueueBody {
            queue: Loaded::Ready(queue),
            selected_site: None,
            rate_toast: None,
        }),
    }
}
