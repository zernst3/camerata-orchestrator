#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Click-through tests of the review lanes (review UI spec sections 13 and 14).
//!
//! Each test starts a fresh `camerata seed-review` server on a free port, the liveview build in
//! front of it and a headless Chrome on the page, then clicks and types like a reviewer and reads
//! what the screen and the server's own files say. Each ends by asserting the shell counts.
//!
//! Run them with `scripts/clickthrough.sh`. They need Google Chrome (a missing Chrome fails, never
//! skips) and the `liveview-review` feature.

#[path = "clickthrough/support.rs"]
mod support;

use support::{Seed, Stack};

use std::collections::BTreeSet;

use serde_json::Value;
use support::{fixture_dir, Strip};

/// A node id as the DOM names it: the hex of its bytes (`camerata_ui_core::tree::dom_token`).
fn token(id: &str) -> String {
    id.bytes().map(|b| format!("{b:02x}")).collect()
}

/// The seeded fixture, read from its own files, so what a test expects is worked out from the
/// data and not from a number someone typed once.
struct Model {
    run: Value,
    overlays: Value,
    clean: Value,
}

impl Model {
    fn load() -> Self {
        let read = |f: &str| -> Value {
            let text = std::fs::read_to_string(fixture_dir().join(f)).expect("fixture file");
            serde_json::from_str(&text).expect("fixture json")
        };
        Self {
            run: read("run.json"),
            overlays: read("overlays.json"),
            clean: read("clean.json"),
        }
    }

    fn sites(&self) -> Vec<&Value> {
        let kept = self.run["sites"].as_array().expect("sites");
        let thrown = self.run["thrown_out"].as_array().expect("thrown_out");
        kept.iter().chain(thrown).collect()
    }

    fn site(&self, id: &str) -> &Value {
        self.sites()
            .into_iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("no fixture site {id}"))
    }

    /// The rows the release gate refuses in the base variant.
    fn held(&self) -> BTreeSet<String> {
        self.overlays["expected_held"]["base"]
            .as_array()
            .expect("held list")
            .iter()
            .map(|h| h["site"].as_str().expect("a site").to_string())
            .collect()
    }

    /// The site decisions the base overlays make, as (site, state).
    fn decisions(&self) -> Vec<(String, String)> {
        self.overlays["decisions"]
            .as_array()
            .expect("decisions")
            .iter()
            .filter_map(|d| {
                Some((
                    d["site"].as_str()?.to_string(),
                    d["state"].as_str()?.to_string(),
                ))
            })
            .collect()
    }

    /// What each site's decision says before the test touches it: the state of its site
    /// decision in the base overlays, `merged` for a reviewer's merge, nothing for no decision.
    fn prior_states(&self) -> std::collections::BTreeMap<String, String> {
        let mut out: std::collections::BTreeMap<String, String> = self
            .decisions()
            .into_iter()
            .filter(|(_, state)| state != "engine_restored")
            .collect();
        for m in self.overlays["merges"].as_array().expect("merges") {
            out.insert(
                m["site"].as_str().expect("a site").to_string(),
                "merged".to_string(),
            );
        }
        for e in self.overlays["engine_check"]
            .as_array()
            .expect("engine_check")
        {
            if let Some(c) = e["correct_with"].as_object() {
                out.insert(
                    e["site"].as_str().expect("a site").to_string(),
                    c["state"].as_str().expect("a state").to_string(),
                );
            }
        }
        out
    }

    /// The sites the group reviewer or the evidence check marks as outliers.
    fn outliers(&self) -> BTreeSet<String> {
        self.run["stage2"]["outliers"]
            .as_array()
            .expect("outliers")
            .iter()
            .map(|o| o["site"].as_str().expect("a site").to_string())
            .chain(
                self.sites()
                    .into_iter()
                    .filter(|s| s["evidence_unverified"].is_string())
                    .map(|s| s["id"].as_str().expect("an id").to_string()),
            )
            .collect()
    }

    /// A decision state as the server writes it: a word, or `{"merged_into": key}`.
    fn state_name(state: &Value) -> String {
        match state {
            Value::String(s) => s.clone(),
            Value::Object(o) if o.contains_key("merged_into") => "merged".to_string(),
            _ => String::new(),
        }
    }

    /// The counts the clean variant opens with: the base counts, with every site the clean
    /// overlay decides, merges or corrects moved out of Undecided and into its new bucket.
    fn clean_strip(&self) -> Strip {
        let mut strip = BASE;
        let prior = self.prior_states();
        let mut moved: Vec<(String, String)> = Vec::new();
        for d in self.clean["decisions"].as_array().expect("decisions") {
            let state = d["state"].as_str().expect("a state");
            moved.push((
                d["site"].as_str().expect("a site").to_string(),
                state.to_string(),
            ));
        }
        for m in self.clean["merges"].as_array().expect("merges") {
            moved.push((
                m["site"].as_str().expect("a site").to_string(),
                "merged".to_string(),
            ));
        }
        for v in self.clean["engine_check"].as_array().expect("engine_check") {
            if let Some(c) = v["correct_with"].as_object() {
                moved.push((
                    v["site"].as_str().expect("a site").to_string(),
                    c["state"].as_str().expect("a state").to_string(),
                ));
            }
        }
        for (site, to) in moved {
            // The base counts the held sites as undecided; the restored thrown-out site is not
            // among the 31 and keeps its own bucket.
            strip.shift(prior.get(&site).map(String::as_str), &to);
        }
        strip.held = 0;
        strip
    }

    /// The sites of the rule's one sub-group: its kept sites and the sites the engine threw out
    /// that the reviewer put back.
    fn subgroup_of_rule(&self, rule: &str) -> Vec<String> {
        let restored: BTreeSet<String> = self
            .decisions()
            .into_iter()
            .filter(|(_, state)| state == "engine_restored")
            .map(|(site, _)| site)
            .collect();
        self.sites()
            .into_iter()
            .filter(|s| s["rule_id"] == rule)
            .map(|s| s["id"].as_str().expect("an id").to_string())
            .filter(|id| {
                let thrown = self.site(id)["reason"].is_string();
                !thrown || restored.contains(id)
            })
            .collect()
    }
}

#[test]
fn liveview_serves_only_the_review_lanes() {
    let stack = Stack::open(Seed::Base);
    // The five lanes of the review shell, in order, and nothing of the cockpit around them.
    let tabs = stack.js_string(
        "T.all('.triage-tab').map(e => e.innerText.trim().split(/\\s+\\d/)[0]).join('|')",
    );
    assert_eq!(tabs, "Tree|Escalations|Engine Check|Flat view|Settings");
    assert_eq!(stack.js("T.count('.review-shell')"), 1);
    for foreign in [
        ".cockpit-nav",
        ".cockpit-nav-tab",
        ".chat-bubble",
        ".terminal-bubble",
        ".onboard-panel",
    ] {
        assert_eq!(
            stack.js(&format!("T.count({})", support::js_str(foreign))),
            0,
            "{foreign} is a cockpit screen, and this build serves only the review lanes"
        );
    }
    let page = stack.page_text();
    for screen in ["Routines", "Workspace", "Governed Development", "Rules"] {
        assert!(
            !page.contains(screen),
            "the page names the {screen} screen of the cockpit:\n{page}"
        );
    }
    // The routes: the page and its live connection, the platform's two and nothing else.
    assert_eq!(support::get_status(&stack.page).0, 200);
    for other in ["/api/projects", "/cockpit", "/chat", "/assets/app.js"] {
        assert_eq!(
            support::get_status(&format!("{}{other}", stack.page)).0,
            404,
            "{other} must not be served"
        );
    }
    stack.assert_shell_counts(&Strip {
        unresolved: 15,
        total: 31,
        confirmed: 11,
        ignored: 1,
        false_positive: 2,
        tech_debt: 1,
        merged: 1,
        held: 5,
    });
    stack.assert_no_model_calls();
}

/// Open the tree down to the sub-group of `rule` under `category`, with a real click on each
/// chevron, and wait for its sites.
fn open_subgroup(stack: &Stack, category: &str, rule: &str, subgroup: &str, sites: usize) {
    for node in [category, rule, subgroup] {
        let sel = format!(".tree-node[data-node=\"{}\"] .tree-chevron", token(node));
        stack.click(&sel, "");
        // The next node (or the sites) draws once this one is open.
    }
    stack.wait_for(
        "the sub-group's sites",
        &format!("T.count('.site-row') >= {sites}"),
    );
}

#[test]
fn tree_descend_and_batch_confirm_with_two_outliers_ticked() {
    let model = Model::load();
    let stack = Stack::open(Seed::Base);
    let rule = "SEC-NO-RAW-SQL-CONCAT-1";
    let subgroup = format!("sub:{rule}/0");

    // What the batch covers, worked out from the fixture: the sub-group's sites, less the held
    // rows (always excluded), the ones that already say Confirmed and the row a merge took
    // out of play (the server never overwrites a merge). The two outliers are ticked; one of
    // them is held, so it stays out and the other goes in.
    let members = model.subgroup_of_rule(rule);
    let held = model.held();
    let prior = model.prior_states();
    let outliers: BTreeSet<String> = model
        .outliers()
        .into_iter()
        .filter(|o| members.contains(o))
        .collect();
    assert_eq!(
        outliers.len(),
        2,
        "the fixture has two outliers in the sub-group: {outliers:?}"
    );
    let in_plan: Vec<&String> = members
        .iter()
        .filter(|m| !held.contains(*m) && prior.get(*m).map(String::as_str) != Some("confirmed"))
        .collect();
    let covered: Vec<&String> = in_plan
        .iter()
        .copied()
        .filter(|m| prior.get(*m).map(String::as_str) != Some("merged"))
        .collect();
    let held_here = members.iter().filter(|m| held.contains(*m)).count();
    let already = members
        .iter()
        .filter(|m| !held.contains(*m) && prior.get(*m).map(String::as_str) == Some("confirmed"))
        .count();
    assert_eq!(
        (
            members.len(),
            held_here,
            already,
            in_plan.len(),
            covered.len()
        ),
        (13, 5, 2, 6, 5),
        "the fixture's arithmetic"
    );

    open_subgroup(
        &stack,
        "cat:injection",
        &format!("rule:{rule}"),
        &subgroup,
        members.len(),
    );
    assert_eq!(
        stack.js("T.count('.site-row .col-tick input')"),
        2,
        "an outlier row has a tick"
    );

    // Tick both outliers, then put the cursor on the sub-group and ask to confirm all of it.
    for _ in 0..2 {
        stack.click(".site-row .col-tick input:not(:checked)", "");
    }
    stack.wait_for(
        "both ticks to stick",
        "T.count('.site-row .col-tick input:checked') === 2",
    );
    let before = stack.store("decisions")["decisions"]
        .as_array()
        .expect("decisions")
        .len();
    let strip_before = stack.strip();
    stack.click(
        &format!(".tree-node[data-node=\"{}\"] .tree-label", token(&subgroup)),
        "",
    );
    stack.click(".tree-right .pane-dispose", "Confirm all");
    stack.wait_for("the plan", "T.has('.batch-sentence', 'This writes')");
    let sentence = stack.text(".batch-sentence");
    // The plan counts the merged row among the writes (the server leaves it alone): see the
    // report. Everything else in the sentence is the fixture's arithmetic.
    assert!(
        sentence.starts_with(&format!("This writes {} site records.", in_plan.len())),
        "{sentence}"
    );
    assert!(
        sentence.contains("1 ticked outlier is included."),
        "{sentence}"
    );
    assert!(
        sentence.contains(&format!("{held_here} held rows are excluded.")),
        "{sentence}"
    );
    assert!(
        sentence.contains(&format!("{already} already hold this decision")),
        "{sentence}"
    );

    // The reason, and commit.
    let reason = "every query here concatenates a request value into the SQL text";
    stack.type_into(".queue-reason-input", "", reason);
    stack.click(".queue-prompt-commit", "");
    stack.wait_for("the undo bar", "T.has('.tree-undo-dock', 'Undo')");

    // The decisions file: the node record (Confirmed, with the reason, covering exactly the sites
    // worked out above) and one site record for each of them, derived from it.
    let decisions = stack.store("decisions");
    let all = decisions["decisions"].as_array().expect("decisions");
    assert_eq!(
        all.len(),
        before + 1 + covered.len(),
        "a node record and a site record each"
    );
    let node_rec = &all[before];
    assert_eq!(node_rec["state"], "confirmed", "{node_rec}");
    assert_eq!(node_rec["reason"], reason, "{node_rec}");
    assert_eq!(node_rec["scope"]["node"], subgroup.as_str(), "{node_rec}");
    assert_eq!(
        node_rec["applies_to"].as_array().expect("applies_to").len(),
        covered.len(),
        "{node_rec}"
    );
    for rec in &all[before + 1..] {
        assert_eq!(rec["state"], "confirmed", "{rec}");
        assert_eq!(rec["scope"], "site", "{rec}");
        assert_eq!(rec["inherited_from"], node_rec["id"], "{rec}");
    }

    // ... and the server's own sites read says each of those sites is now Confirmed and no
    // other site changed state.
    let page = support::get_json(&format!(
        "{}/review/sites?node={}&limit=50",
        stack.api, subgroup
    ));
    let rows = page["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), members.len());
    for site in &members {
        let spec = model.site(site);
        let row = rows
            .iter()
            .find(|r| r["path"] == spec["path"] && r["line"] == spec["line"])
            .unwrap_or_else(|| panic!("{site} is on the page"));
        let state_now = Model::state_name(&row["decision"]["state"]);
        let expected = if covered.contains(&site) {
            "confirmed".to_string()
        } else {
            prior.get(site).cloned().unwrap_or_default()
        };
        assert_eq!(state_now, expected, "{site} ({})", row["headline"]);
    }

    // The shell counts: each covered site left the bucket it was in and became Confirmed.
    let mut after = strip_before;
    for site in &covered {
        after.shift_to_confirmed(prior.get(*site).map(String::as_str));
    }
    assert_eq!(
        after.confirmed,
        strip_before.confirmed + covered.len(),
        "every covered site joined Confirmed (none was there already)"
    );
    stack.assert_shell_counts(&after);
    stack.assert_no_model_calls();
}

/// The counts the base seed opens with.
const BASE: Strip = Strip {
    unresolved: 15,
    total: 31,
    confirmed: 11,
    ignored: 1,
    false_positive: 2,
    tech_debt: 1,
    merged: 1,
    held: 5,
};

#[test]
fn lowering_the_line_sends_nothing_but_the_queue_reload() {
    let stack = Stack::open(Seed::Base);
    let files = stack.data_fingerprint();

    // Opening the lane reads the queue, once. That is the reload.
    let opened = stack.mark();
    stack.click(".triage-tab", "Escalations");
    stack.wait_text(
        ".review-queue-budget",
        "Budget 2 of 10,000 lines at 0.0 per 1,000 (floor 2)",
    );
    let reads = stack.requests_since(opened);
    assert!(
        reads
            .iter()
            .any(|r| r.method == "GET" && r.path().ends_with("/review/queue")),
        "the lane reads the queue: {reads:?}"
    );
    assert!(
        reads
            .iter()
            .all(|r| r.method == "GET" && r.path().ends_with("/review/queue")),
        "opening the lane reads the queue and nothing else: {reads:?}"
    );
    assert_eq!(
        stack.js("T.count('.mark-cand')"),
        0,
        "the candidates are hidden"
    );
    assert!(stack
        .text(".review-queue-below")
        .contains("2 candidates (hidden)"));

    // Lowering the line is a look: two candidates appear, and no request leaves.
    let before_look = stack.mark();
    stack.click(".review-queue-below button", "Show below the line");
    stack.wait_for("the candidates", "T.count('.mark-cand') === 2");
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(
        stack.requests_since(before_look),
        vec![],
        "showing the candidates below the line must send nothing"
    );
    // The budget sentence did not move: this was not a change of rate.
    assert_eq!(
        stack.text(".review-queue-budget"),
        "Budget 2 of 10,000 lines at 0.0 per 1,000 (floor 2)"
    );
    stack.click(".review-queue-below button", "Hide the candidates");
    stack.wait_for("the candidates to hide", "T.count('.mark-cand') === 0");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        stack.requests_since(before_look).is_empty(),
        "hiding them sends nothing either"
    );

    // Nothing was written: every file the server holds is as the seed left it, and the shell
    // counts are the seed's.
    assert_eq!(
        stack.data_fingerprint(),
        files,
        "the server's files are untouched"
    );
    stack.assert_shell_counts(&BASE);
    stack.assert_no_model_calls();
}

/// Go to the Engine Check card that shows `place` (a `path:line`), with real clicks on Next.
fn go_to_card(stack: &Stack, place: &str) {
    stack.click(".triage-tab", "Engine Check");
    stack.wait_for("the first card", "T.count('.engine-card') === 1");
    for _ in 0..40 {
        if stack.text(".engine-card .site-pane-code").trim() == place {
            return;
        }
        let at = stack.text(".engine-card-pos");
        stack.click(".engine-card-keys button", "Next");
        stack.wait_for(
            "the next card",
            &format!("T.text('.engine-card-pos') !== {}", support::js_str(&at)),
        );
    }
    panic!("no Engine Check card shows {place}:\n{}", stack.page_text());
}

#[test]
fn engine_check_no_wrong_severity_change_it_and_the_row_is_no_longer_held() {
    let stack = Stack::open(Seed::Base);
    let model = Model::load();
    // The row the base seed holds because its No has no correction: sql-11.
    let site = model.site("sql-11");
    let place = format!("{}:{}", site["path"].as_str().unwrap(), site["line"]);
    let key = stack
        .gate_held()
        .into_iter()
        .find(|h| h["reason"] == "engine_check_no_without_correction")
        .map(|h| h["key"].as_str().unwrap().to_string())
        .expect("the gate holds the row for its uncorrected No");

    go_to_card(&stack, &place);
    stack.wait_text(
        ".engine-state-text",
        "held from export until corrected (wrong severity)",
    );
    assert_eq!(stack.strip().held, 5);

    // Correct it: it was wrong about the severity, and the right one is critical.
    stack.click(".engine-card button", "Correct it");
    stack.wait_for("the correction form", "T.count('.engine-correction') === 1");
    assert!(
        stack
            .text(".engine-correction .rule-modal-label")
            .to_lowercase()
            .starts_with("no: wrong severity."),
        "{}",
        stack.text(".engine-correction")
    );
    assert_eq!(
        stack.js_string("T.find('.engine-correction select').value"),
        "high",
        "the form starts on the row's own severity"
    );
    stack.js("(() => { const s = T.find('.engine-correction select'); s.value = 'critical'; s.dispatchEvent(new Event('change', { bubbles: true })); return s.value; })()");
    let reason = "the key lets anyone read every customer record, which is critical";
    stack.type_into(".engine-correction .queue-reason-input", "", reason);
    stack.click(".engine-correction .queue-prompt-commit", "");

    // The card says corrected, and the Held count fell by the one row.
    stack.wait_text(".engine-state-text", "corrected");
    stack.wait_for("no correction form", "T.count('.engine-correction') === 0");
    assert!(stack
        .page_text()
        .contains("The corrected version is what ships."));
    assert!(
        !stack.gate_held().iter().any(|h| h["key"] == key.as_str()),
        "the gate no longer holds the row"
    );
    let gate_text = stack.js_string("T.text('.export-blocked-head')");
    assert!(
        gate_text.contains("4 rows"),
        "the export panel lists one row fewer: {gate_text}"
    );

    // The correction is a real severity edit on that row, with the reason, and the verdict names it.
    let edits = stack.store("edits");
    let edit = edits["edits"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(edit["key"], key.as_str(), "{edit}");
    assert_eq!(edit["field"], "severity", "{edit}");
    assert_eq!(edit["new"], "critical", "{edit}");
    assert_eq!(edit["reason"], reason, "{edit}");
    let verdicts = stack.store("engine-check");
    // The store appends: the latest verdict on the row is the one that counts.
    let verdict = verdicts["verdicts"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|v| v["key"] == key.as_str())
        .unwrap_or_else(|| panic!("no verdict for the row in {verdicts}"))
        .clone();
    assert_eq!(verdict["right"], false, "{verdict}");
    assert_eq!(verdict["wrong"], "wrong_severity", "{verdict}");
    assert_eq!(verdict["correction"]["edit"], edit["id"], "{verdict}");

    stack.assert_shell_counts(&Strip { held: 4, ..BASE });
    stack.assert_no_model_calls();
}

/// Open the tree on a rule's one sub-group and put the cursor on the site at `place`
/// (`path:line`), so the detail pane shows it.
fn open_site(stack: &Stack, category: &str, rule: &str, place: &str) {
    let subgroup = format!("sub:{rule}/0");
    open_subgroup(stack, category, &format!("rule:{rule}"), &subgroup, 1);
    stack.click(".site-row .col-loc", place);
    stack.wait_for(
        "the detail pane",
        &format!(
            "T.has('.site-detail .site-detail-where', {})",
            support::js_str(place)
        ),
    );
}

/// Click Export (the gate must be clear), script the save dialog, and return the zip the app
/// saved through the platform.
fn export_zip(stack: &Stack) -> Vec<u8> {
    let path = stack
        .data
        .path()
        .join(format!("export-{}.zip", stack.platform_calls().len()));
    stack.script_platform(&serde_json::json!({ "saves": [path] }));
    stack.click(".export-clear .btn-run", "Export product");
    stack.wait_for(
        "the save dialog to have been asked",
        "T.has('.toast-host', 'Product export saved.')",
    );
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("the app saved nothing at {}: {e}", path.display()))
}

/// Every text part of an xlsx inside the export zip (shared strings and sheets), as one string.
fn workbook_text(zip_bytes: &[u8]) -> String {
    use std::io::Read;
    let mut outer = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("a zip");
    let name = outer
        .file_names()
        .find(|n| n.ends_with(".xlsx"))
        .expect("the export holds a workbook")
        .to_string();
    let mut book = Vec::new();
    outer
        .by_name(&name)
        .expect("the workbook")
        .read_to_end(&mut book)
        .expect("readable");
    let mut inner = zip::ZipArchive::new(std::io::Cursor::new(book)).expect("an xlsx is a zip");
    let mut text = String::new();
    for i in 0..inner.len() {
        let mut part = inner.by_index(i).expect("a part");
        if part.name().ends_with(".xml") {
            let mut xml = String::new();
            let _ = part.read_to_string(&mut xml);
            text.push_str(&xml);
        }
    }
    text
}

#[test]
fn edit_a_headline_and_the_workbook_export_carries_it() {
    let model = Model::load();
    let stack = Stack::open(Seed::Clean);
    let clean = model.clean_strip();
    stack.assert_shell_counts(&clean);
    // The gate is clear: the Export button is there and no blocked panel.
    assert_eq!(stack.js("T.count('.export-blocked')"), 0);
    assert_eq!(stack.js("T.count('.export-clear .btn-run')"), 1);

    // Edit the headline of sql-05 in the tree.
    let site = model.site("sql-05");
    let place = format!("{}:{}", site["path"].as_str().unwrap(), site["line"]);
    let old = site["title"].as_str().unwrap().to_string();
    open_site(&stack, "cat:injection", "SEC-NO-RAW-SQL-CONCAT-1", &place);
    let new = "Export builds a table name from a deployment setting, never from a request";
    stack.click_in(".site-detail-line", "Headline", ".site-detail-edit");
    stack.wait_for(
        "the headline form",
        "T.has('.site-detail-form .rule-modal-label', 'Edit the headline')",
    );
    stack.type_into(".site-detail-form .site-detail-input", "", new);
    let reason = "the table name comes from the deployment, not from the caller";
    stack.type_into(".site-detail-form .queue-reason-input", "", reason);
    stack.click(".site-detail-form .queue-prompt-commit", "");
    stack.wait_text(".site-detail", new);

    // The edit is on file, on that row, with the reason.
    let edits = stack.store("edits");
    let edit = edits["edits"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(edit["field"], "headline", "{edit}");
    assert_eq!(edit["old"], old.as_str(), "{edit}");
    assert_eq!(edit["new"], new, "{edit}");
    assert_eq!(edit["reason"], reason, "{edit}");

    // The export carries it: the workbook's text has the new headline, and the data.json too.
    let zip_bytes = export_zip(&stack);
    let sheets = workbook_text(&zip_bytes);
    assert!(
        sheets.contains(new),
        "the workbook does not carry the edited headline"
    );
    {
        use std::io::Read;
        let mut outer = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes)).unwrap();
        let names: Vec<String> = outer.file_names().map(str::to_string).collect();
        let name = names
            .iter()
            .find(|n| n.ends_with(".json") && !n.contains("manifest"))
            .unwrap_or_else(|| panic!("no json in the export: {names:?}"))
            .clone();
        let mut data = String::new();
        outer
            .by_name(&name)
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert!(
            data.contains(new),
            "data.json does not carry the edited headline"
        );
    }
    // The save went through the platform's dialog, once, and wrote what the server sent.
    let calls = stack.platform_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["call"], "save_file", "{calls:?}");
    assert_eq!(calls[0]["outcome"], "Saved", "{calls:?}");
    assert!(
        calls[0]["default_name"].as_str().unwrap().ends_with(".zip"),
        "{calls:?}"
    );
    assert_eq!(calls[0]["bytes"], zip_bytes.len(), "{calls:?}");

    stack.assert_shell_counts(&clean);
    stack.assert_no_model_calls();
}

/// The place (`path:line`) of a fixture site.
fn place_of(model: &Model, id: &str) -> String {
    let site = model.site(id);
    format!("{}:{}", site["path"].as_str().unwrap(), site["line"])
}

#[test]
fn merge_into_a_target_and_the_row_remains() {
    let model = Model::load();
    let stack = Stack::open(Seed::Base);
    let rule = "SEC-NO-RAW-SQL-CONCAT-1";
    let (source, target) = (place_of(&model, "sql-04"), place_of(&model, "sql-03"));
    open_site(&stack, "cat:injection", rule, &source);
    let rows_before = stack.js("T.count('.site-row')").as_u64().unwrap();
    assert_eq!(rows_before, 13);

    stack.click(".site-detail-merge", "");
    stack.wait_for("the merge form", "T.count('.site-detail-merge-form') === 1");
    // Search for the target, pick it, say why.
    stack.type_into(".site-detail-merge-form .site-detail-input", "", &target);
    stack.click(".site-detail-candidate", &target);
    stack.wait_for("the pick", "T.count('.site-detail-candidate.picked') === 1");
    let reason = "the same status value reaches both filters";
    stack.type_into(".site-detail-merge-form .queue-reason-input", "", reason);
    stack.click(".site-detail-merge-form .queue-prompt-commit", "");

    // The row is still in the tree, now reading Merged into its target.
    stack.wait_for(
        "the merge on the row",
        &format!(
            "T.has('.site-row', {}) && T.find('.site-row', {}).querySelector('.col-dec').textContent.includes('Merged into')",
            support::js_str(&source),
            support::js_str(&source)
        ),
    );
    assert_eq!(
        stack.js("T.count('.site-row')").as_u64().unwrap(),
        rows_before,
        "no row left the tree"
    );
    let page = support::get_json(&format!(
        "{}/review/sites?node=sub:{rule}/0&limit=50",
        stack.api
    ));
    let rows = page["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 13);
    let row_of = |id: &str| {
        let spec = model.site(id);
        rows.iter()
            .find(|r| r["path"] == spec["path"] && r["line"] == spec["line"])
            .unwrap_or_else(|| panic!("{id} is on the page"))
            .clone()
    };
    let (merged, into) = (row_of("sql-04"), row_of("sql-03"));
    assert_eq!(
        merged["decision"]["state"]["merged_into"], into["site_key"],
        "the record names the target: {merged}"
    );
    assert_eq!(merged["decision"]["reason"], reason);
    // The target is untouched.
    assert_eq!(into["decision"]["state"], "confirmed");

    // The decisions file holds the merge as a site decision on the source, with the reason.
    let decisions = stack.store("decisions");
    let rec = decisions["decisions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(rec["key"], merged["site_key"], "{rec}");
    assert_eq!(rec["state"]["merged_into"], into["site_key"], "{rec}");
    assert_eq!(rec["reason"], reason, "{rec}");

    // sql-04 was undecided and is now merged.
    let mut expected = BASE;
    expected.shift(None, "merged");
    stack.assert_shell_counts(&expected);
    stack.assert_no_model_calls();
}

#[test]
fn a_rerun_with_a_moved_line_shows_the_stale_chip_and_reapply_clears_it() {
    let model = Model::load();
    let stack = Stack::open(Seed::Moved);
    let rule = "SEC-NO-RAW-SQL-CONCAT-1";
    let stale_of = |stack: &Stack| {
        let shell = support::get_json(&format!("{}/review", stack.api));
        (
            shell["stale"]["stale_decisions"].as_u64().unwrap(),
            shell["stale"]["stale_edits"].as_u64().unwrap(),
            shell["stale"]["moved"].as_u64().unwrap(),
            shell["stale"]["new_since_decision"].as_u64().unwrap(),
        )
    };
    // The rerun moved two rows and left one decision and one edit behind on their old keys.
    assert_eq!(stale_of(&stack), (1, 1, 2, 1));
    let opening = stack.strip();
    assert_eq!(opening.total, 32, "the first run's 31 rows and the new one");

    // The moved rows are in the tree with their chips.
    let (decided, edited) = (moved_place(&model, "sql-03"), moved_place(&model, "sql-04"));
    open_subgroup(
        &stack,
        "cat:injection",
        &format!("rule:{rule}"),
        &format!("sub:{rule}/0"),
        1,
    );
    let row_chips = |place: &str| {
        stack.js_string(&format!(
            "(() => {{ const r = T.find('.site-row', {}); return r ? r.querySelector('.col-markers').textContent : ''; }})()",
            support::js_str(place)
        ))
    };
    assert!(
        row_chips(&decided).contains("stale: was Confirmed"),
        "{}",
        row_chips(&decided)
    );
    assert!(
        row_chips(&decided).contains("moved, was line 19"),
        "{}",
        row_chips(&decided)
    );
    assert!(
        row_chips(&edited).contains("stale edit"),
        "{}",
        row_chips(&edited)
    );
    assert!(
        row_chips(&edited).contains("moved, was line 58"),
        "{}",
        row_chips(&edited)
    );
    assert_eq!(stack.js("T.count('.site-row .mark-stale')"), 2);

    // Reapply the stale edit: it is posted again as a new edit on the row's new key, and the
    // chip goes.
    let before = stack.mark();
    stack.click(".site-row .col-loc", &edited);
    stack.wait_for("the stale edit", "T.count('.site-detail-stale') === 1");
    stack.click(".site-detail-stale .site-detail-line-action", "Reapply");
    stack.wait_for(
        "the stale edit to clear",
        "T.count('.site-detail-stale') === 0",
    );
    let posts: Vec<_> = stack
        .requests_since(before)
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0].path().contains("/edits/") && posts[0].path().ends_with("/reapply"),
        "{posts:?}"
    );
    let edits = stack.store("edits");
    let latest = edits["edits"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(latest["field"], "headline", "{latest}");
    assert_eq!(
        latest["new"], "Search sort builds SQL from the sort column of the request",
        "{latest}"
    );
    assert!(
        latest["reapplied_from"].is_string(),
        "the new edit says what it reapplies: {latest}"
    );
    stack.wait_for("the row's chip", "T.count('.site-row .mark-stale') === 1");
    assert!(
        row_chips(&edited).contains("edited"),
        "{}",
        row_chips(&edited)
    );
    assert_eq!(
        stale_of(&stack),
        (1, 0, 2, 1),
        "the edit is no longer stale"
    );

    // Reapply the stale decision: the row is Confirmed again, as it was, on the new key.
    stack.click(".site-row .col-loc", &decided);
    stack.wait_for(
        "the stale decision",
        "T.count('.site-detail-decision .site-detail-line-action') === 2",
    );
    stack.click(".site-detail-decision .site-detail-line-action", "Reapply");
    stack.wait_for(
        "the stale chip to go",
        "T.count('.site-row .mark-stale') === 0",
    );
    let decisions = stack.store("decisions");
    let rec = decisions["decisions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(rec["state"], "confirmed", "{rec}");
    assert_eq!(
        rec["reason"], "the status comes straight from the query string",
        "{rec}"
    );
    assert_eq!(
        stale_of(&stack),
        (0, 0, 2, 1),
        "nothing is stale any more; two rows are still moved"
    );

    // One row went from Undecided to Confirmed.
    let mut expected = opening;
    expected.shift(None, "confirmed");
    stack.assert_shell_counts(&expected);
    stack.assert_no_model_calls();
}

/// The place of a fixture site in the moved variant's second run.
fn moved_place(model: &Model, id: &str) -> String {
    let moved: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("moved.json")).expect("moved.json"),
    )
    .expect("moved json");
    let site = model.site(id);
    let line = moved["moves"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["site"] == id)
        .and_then(|m| m["line"].as_u64())
        .unwrap_or_else(|| site["line"].as_u64().unwrap());
    format!("{}:{line}", site["path"].as_str().unwrap())
}

/// The text of the export's PDF, where the methodology prints the ledger's escalation block.
fn pdf_text(zip_bytes: &[u8]) -> String {
    use std::io::Read;
    let mut outer = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("a zip");
    let name = outer
        .file_names()
        .find(|n| n.ends_with(".pdf"))
        .expect("the export holds a PDF")
        .to_string();
    let mut pdf = Vec::new();
    outer
        .by_name(&name)
        .expect("the PDF")
        .read_to_end(&mut pdf)
        .expect("readable");
    pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts")
}

#[test]
fn changing_the_rate_moves_the_budget_sentence_and_the_ledger_block() {
    let model = Model::load();
    let stack = Stack::open(Seed::Clean);
    let clean = model.clean_strip();
    stack.assert_shell_counts(&clean);

    // What the new line must be, from the fixture's own numbers: rate 2.0 per 1,000 lines over
    // the run's counted lines, never under the floor; all six candidates then sit inside it.
    let lines = model.run["run"]["code_lines"].as_u64().unwrap();
    let floor = model.overlays["settings"]["floor"].as_u64().unwrap();
    let candidates = model.run["stage2"]["candidates"].as_array().unwrap().len() as u64;
    let rate = 2.0_f64;
    let budget = (rate * lines as f64 / 1000.0).round() as u64;
    let budget = budget.max(floor);
    let escalated = candidates.min(budget);
    assert_eq!((budget, escalated), (20, 6), "the fixture's arithmetic");

    // Settings: type the rate, see the preview, apply, read the toast.
    stack.click(".triage-tab", "Settings");
    stack.wait_for("the settings", "T.count('#rs-rate') === 1");
    stack.type_into("#rs-rate", "", "2.0");
    stack.wait_text(".review-settings-budget", &format!("budget {budget}"));
    let mark = stack.mark();
    stack.click(".review-settings-apply", "");
    stack.wait_text(".toast-host", "No model call was made.");
    let posts: Vec<_> = stack
        .requests_since(mark)
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "one write: {posts:?}");
    assert!(posts[0].path().ends_with("/escalation-budget"), "{posts:?}");
    let toast = stack.text(".toast-host");
    assert!(
        toast.contains(&format!(
            "{} candidates moved above the line.",
            candidates - 4
        )),
        "two candidates were below the line and now are not: {toast}"
    );

    // The queue's budget sentence and counts follow.
    stack.click(".triage-tab", "Escalations");
    stack.wait_text(
        ".review-queue-budget",
        &format!("Budget {budget} of 10,000 lines at 2.0 per 1,000 (floor {floor})"),
    );
    let counts = stack.text(".review-queue-counts");
    assert!(
        counts.starts_with(&format!("Escalated {escalated}")),
        "{counts}"
    );
    assert!(
        !counts.contains("over budget"),
        "nothing is over a budget of {budget}: {counts}"
    );
    assert!(stack
        .text(".triage-tab:nth-child(2)")
        .contains(&escalated.to_string()));

    // The ledger block of the export: the PDF states the escalation outcome, derived from the
    // line as it stands now (a budget of 20 and none over), and not the old one (a budget of 2).
    let zip_bytes = export_zip(&stack);
    let text = pdf_text(&zip_bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let sentence = format!("Escalated {escalated} of a budget of {budget} (none over)");
    assert!(
        text.contains(&sentence),
        "the PDF does not say {sentence:?}"
    );
    assert!(
        !text.contains("of a budget of 2 ("),
        "the PDF still carries the old line"
    );

    // Changing the line changes no decision: the counts are the clean variant's.
    stack.assert_shell_counts(&clean);
    stack.assert_no_model_calls();
}

#[test]
fn adding_a_finding_with_a_stale_snippet_is_held_and_the_gate_refuses_the_export() {
    let model = Model::load();
    let stack = Stack::open(Seed::Clean);
    let clean = model.clean_strip();
    stack.assert_shell_counts(&clean);
    assert_eq!(
        stack.js("T.count('.export-blocked')"),
        0,
        "the gate starts clear"
    );

    // The code the reviewer will quote, on disk where the app reads it from.
    let first = "pub fn delete_session(token: &str) {\n";
    let offending = "    let q = format!(\"DELETE FROM sessions WHERE token = '{}'\", token);\n";
    let clone = stack.link_repo(
        "acme/portal",
        &[(
            "src/orders/repo.rs",
            &format!("// sessions\nuse std::fmt;\n\n{first}{offending}}}\n"),
        )],
    );

    // Open a site (the Add button lives in the detail pane) and open the form.
    open_site(
        &stack,
        "cat:injection",
        "SEC-NO-RAW-SQL-CONCAT-1",
        &place_of(&model, "sql-01"),
    );
    stack.click(".site-detail-add", "");
    stack.wait_for("the add form", "T.count('.site-detail-add-form') === 1");

    // Fill it in: the repo, the file, the line the code is on, the rule, the words.
    let fill = |nth: usize, value: &str| {
        stack.js(&format!(
            "(() => {{ const e = T.all('.site-detail-add-form input.addressee-input, .site-detail-add-form textarea.addressee-input')[{nth}]; e.focus(); e.select && e.select(); return true; }})()"
        ));
        stack.tab.send_character(value).expect("typing");
    };
    // Repo, File, Line, Rule, (Kind and Severity are selects), Headline; Detail, Fix and the reason are text areas.
    fill(0, "acme/portal");
    fill(1, "src/orders/repo.rs");
    fill(2, "5");
    fill(3, "SEC-NO-RAW-SQL-CONCAT-1");
    fill(4, "Session delete builds SQL from the token");
    fill(
        5,
        "The token is pasted into the DELETE statement between quotes.",
    );
    let reason = "found reading the session code";
    stack.type_into(".site-detail-add-form .queue-reason-input", "", reason);

    // Without the code loaded the form refuses, and nothing is sent.
    let before = stack.mark();
    stack.click(".site-detail-add-form .queue-prompt-commit", "");
    stack.wait_text(
        ".site-detail-add-form .site-detail-error",
        "Load the code at that line first",
    );
    assert!(
        stack
            .requests_since(before)
            .iter()
            .all(|r| r.method == "GET"),
        "a refused form posts nothing: {:?}",
        stack.requests_since(before)
    );

    // Load the code at the line: it is the quote the finding will carry.
    stack.click(".site-detail-add-form button", "Load code");
    stack.wait_text(".site-detail-snippet", "DELETE FROM sessions WHERE token");

    // The code changes on disk before the reviewer presses Add: the quoted line is gone.
    std::fs::write(
        clone.join("src/orders/repo.rs"),
        format!("// sessions\nuse std::fmt;\n\n{first}    let q = sqlx::query(\"DELETE FROM sessions WHERE token = $1\");\n}}\n"),
    )
    .expect("the file changes");
    stack.click(".site-detail-add-form .queue-prompt-commit", "");

    // The row was added, and the evidence check could not find the quote near the line: the
    // gate now refuses the export and names the row.
    stack.wait_for("the gate's refusal", "T.count('.export-blocked') === 1");
    let held = stack.gate_held();
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0]["reason"], "evidence_unverified", "{held:?}");
    assert_eq!(held[0]["path"], "src/orders/repo.rs", "{held:?}");
    assert_eq!(held[0]["line"], 5, "{held:?}");
    let panel = stack.text(".export-blocked");
    assert!(
        panel.contains("Export blocked: 1 row needs a decision"),
        "{panel}"
    );
    assert!(panel.contains("src/orders/repo.rs:5"), "{panel}");
    assert!(panel.contains("evidence unverified"), "{panel}");
    assert_eq!(
        stack.js("T.count('.export-clear .btn-run')"),
        0,
        "the Export button is gone"
    );
    let added = stack.store("edits");
    let rows = added["added"].as_array().expect("added");
    assert_eq!(rows.len(), 1, "{added}");
    assert_eq!(rows[0]["reason"], reason, "{}", rows[0]);

    stack.assert_shell_counts(&Strip {
        unresolved: clean.unresolved + 1,
        total: clean.total + 1,
        held: 1,
        ..clean
    });
    stack.assert_no_model_calls();
}

#[test]
fn keyboard_descend_opens_the_sub_group_and_focuses_its_first_site() {
    let model = Model::load();
    let stack = Stack::open(Seed::Base);
    let rule = "SEC-NO-RAW-SQL-CONCAT-1";
    // Put the cursor on the tree (one click, as a reviewer does), then use only the keyboard:
    // Down to Injection, Right to open it, Down to the rule, Right, Down to the sub-group,
    // Right, then Enter to go into its sites.
    stack.click(".tree-node .tree-label", "Authorization");
    stack.wait_for("the keys to arm", "T.has('.pane-keys.armed', '')");
    let node_is = |id: &str| {
        format!("T.find('.tree-node.focused', '') && T.find('.tree-node.focused', '').getAttribute('data-node') === {}", support::js_str(&token(id)))
    };
    for (key, then_on) in [
        ("ArrowDown", "cat:injection"),
        ("ArrowRight", ""),
        ("ArrowDown", &format!("rule:{rule}")),
        ("ArrowRight", ""),
        ("ArrowDown", &format!("sub:{rule}/0")),
    ] {
        stack.key(key);
        if !then_on.is_empty() {
            stack.wait_for(&format!("the cursor on {then_on}"), &node_is(then_on));
        } else {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    stack.key("ArrowRight");
    stack.wait_for("the sub-group's sites", "T.count('.site-row') === 13");
    stack.key("Enter");

    // The first site is focused (the first row the server returns) and the right pane shows it.
    stack.wait_for("a focused site", "T.count('.site-row.focused') === 1");
    let focused_place = stack.text(".site-row.focused .col-loc");
    let first_place = stack.text(".site-row .col-loc");
    assert_eq!(
        focused_place, first_place,
        "the first site is the focused one"
    );
    stack.wait_text(".site-detail .site-detail-where", &focused_place);
    // It is one of the sub-group's own sites.
    let members: Vec<String> = model
        .subgroup_of_rule(rule)
        .iter()
        .map(|id| place_of(&model, id))
        .collect();
    assert!(
        members.contains(&focused_place),
        "{focused_place} is not in {members:?}"
    );

    // Navigating by keys changed nothing on the server.
    stack.assert_shell_counts(&BASE);
    stack.assert_no_model_calls();
}

#[test]
fn held_count_rises_and_falls_through_the_engine_check_and_the_export_panel_doors() {
    let model = Model::load();
    let stack = Stack::open(Seed::Clean);
    let clean = model.clean_strip();
    assert_eq!(clean.held, 0);
    stack.assert_shell_counts(&clean);
    assert_eq!(
        stack.js("T.count('.export-clear .btn-run')"),
        1,
        "the Export button is there"
    );

    // Engine Check, card 1: press N, pick "wrong severity" with its digit, change it to high,
    // commit. The Held count stays where it was and the card says corrected.
    stack.click(".triage-tab", "Engine Check");
    stack.wait_for("the first card", "T.count('.engine-card') === 1");
    stack.click(".engine-card-pos", "");
    stack.wait_for("the keys to arm", "T.has('.pane-keys.armed', '')");
    let first = stack.text(".engine-card .site-pane-code");
    stack.key("n");
    stack.wait_for(
        "the list of what was wrong",
        "T.count('.engine-wrong') === 1",
    );
    stack.key("2");
    stack.wait_for("the correction form", "T.count('.engine-correction') === 1");
    stack.js("(() => { const s = T.find('.engine-correction select'); s.value = 'high'; s.dispatchEvent(new Event('change', { bubbles: true })); return s.value; })()");
    stack.type_into(
        ".engine-correction .queue-reason-input",
        "",
        "this one is reachable without signing in",
    );
    stack.click(".engine-correction .queue-prompt-commit", "");
    stack.wait_text(".engine-state-text", "corrected");
    assert_eq!(stack.strip().held, 0, "a corrected No holds nothing");

    // Card 2: press N, start the correction, then go Next without finishing it. The No is
    // stored uncorrected, so Held rises by one and the export panel lists the row.
    stack.click(".engine-card-keys button", "Next");
    stack.wait_for(
        "card 2",
        "T.text('.engine-card-pos').startsWith('Card 2 of')",
    );
    let second = stack.text(".engine-card .site-pane-code");
    assert_ne!(first, second);
    stack.click(".engine-card-pos", "");
    stack.key("n");
    stack.wait_for("the list", "T.count('.engine-wrong') === 1");
    stack.key("2");
    stack.wait_for("the correction form", "T.count('.engine-correction') === 1");
    stack.click(".engine-card-keys button", "Next");
    stack.wait_for(
        "card 3",
        "T.text('.engine-card-pos').startsWith('Card 3 of')",
    );
    let mut held_one = clean;
    held_one.held = 1;
    stack.assert_shell_counts(&held_one);
    stack.wait_for("the export panel", "T.count('.export-blocked') === 1");
    let panel = stack.text(".export-blocked");
    assert!(
        panel.contains("Export blocked: 1 row needs a decision"),
        "{panel}"
    );
    assert!(
        panel.contains(&second),
        "the panel lists the row of card 2: {panel}"
    );
    assert!(
        panel.contains("Engine Check said No, no correction yet"),
        "{panel}"
    );
    assert_eq!(
        stack.js("T.count('.export-clear .btn-run')"),
        0,
        "the Export button is gone"
    );

    // The panel's door on that row takes the reviewer to that card in the Engine Check lane.
    stack.click_in(".gate-row", &second, "button");
    stack.wait_for(
        "card 2 again",
        "T.text('.engine-card-pos').startsWith('Card 2 of')",
    );
    assert_eq!(stack.text(".engine-card .site-pane-code"), second);
    assert!(
        stack.js_bool("T.has('.triage-tab.active', 'Engine Check')"),
        "the Engine Check lane is showing"
    );

    // Correct it: the Held count falls and the Export button is back.
    stack.click(".engine-card button", "Correct it");
    stack.wait_for("the correction form", "T.count('.engine-correction') === 1");
    stack.type_into(
        ".engine-correction .queue-reason-input",
        "",
        "this one is a real finding at critical",
    );
    stack.js("(() => { const s = T.find('.engine-correction select'); s.value = 'critical'; s.dispatchEvent(new Event('change', { bubbles: true })); return s.value; })()");
    stack.click(".engine-correction .queue-prompt-commit", "");
    stack.wait_for(
        "the Export button",
        "T.count('.export-clear .btn-run') === 1",
    );
    stack.assert_shell_counts(&clean);

    // Export: the save dialog goes through the platform, and the package carries the severity
    // edited on card 1.
    let zip_bytes = export_zip(&stack);
    let calls = stack.platform_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        (calls[0]["call"].as_str(), calls[0]["outcome"].as_str()),
        (Some("save_file"), Some("Saved"))
    );
    let (path, line) = first.rsplit_once(':').expect("path:line");
    let all = {
        use std::io::Read;
        let mut outer = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes)).unwrap();
        let mut text = String::new();
        outer
            .by_name("json/all-findings.json")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        serde_json::from_str::<Value>(&text).unwrap()
    };
    let row = all
        .as_array()
        .expect("a list")
        .iter()
        .find(|f| f["path"] == path && f["line"].as_u64() == line.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("{first} is in the export"));
    assert_eq!(
        row["severity"], "high",
        "the edit made on card 1 ships: {row}"
    );
    stack.assert_no_model_calls();
}
