//! corpus-verifier-gui — a THIN Dioxus 0.7 desktop front-end over the CORE.
//!
//! It is deliberately thin: it lists the grounded queue, lets the maintainer pick
//! a rule and fill a prefilled form (by / at / against), shows an inline EXAMPLE
//! of the resulting `[verified]` TOML block, and on "Verify rule" calls the SAME
//! [`corpus_verifier::verify_one`] CORE the CLI uses. A second view runs the bulk
//! [`corpus_verifier::self_source`] flow.
//!
//! Dioxus 0.7 / desktop, matching `crates/ui`'s family version.
//!
//! Each view has a "Dry run" checkbox (ON by default, preview-first): checked, it
//! runs through [`corpus_verifier::DryRunVcs`] (edits the TOML locally + reports the
//! planned PR URL without pushing); unchecked, it runs the real
//! [`corpus_verifier::GitVcs`] path (branch -> push -> `gh pr create --base main`).

use dioxus::prelude::*;

use corpus_verifier::{
    corpus_dir, list_grounded, self_source, today, verified_block_preview, verify_one, DryRunVcs,
    GitVcs,
    GroundedRow,
};

fn main() {
    use dioxus::desktop::{Config, WindowBuilder};
    dioxus::LaunchBuilder::desktop()
        .with_cfg(Config::new().with_window(
            WindowBuilder::new().with_title("Corpus Verifier (maintainer tool)"),
        ))
        .launch(App);
}

/// Which view is showing.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Queue,
    BulkMeta,
}

#[component]
fn App() -> Element {
    let mut view = use_signal(|| View::Queue);
    rsx! {
        div {
            style: "font-family: system-ui, sans-serif; padding: 16px; max-width: 980px; margin: 0 auto;",
            h1 { "Corpus Verifier" }
            p {
                style: "color:#666;",
                "MAINTAINER-ONLY repo tool. Promotes grounded rules to verified via branch + PR into main. "
                "Not part of the shipped app. (GUI runs the DRY-RUN seam: edits TOML locally, reports the planned PR.)"
            }
            div {
                style: "display:flex; gap:8px; margin: 12px 0;",
                button {
                    onclick: move |_| view.set(View::Queue),
                    "Grounded queue"
                }
                button {
                    onclick: move |_| view.set(View::BulkMeta),
                    "Bulk self-source meta rules"
                }
            }
            match view() {
                View::Queue => rsx! { QueueView {} },
                View::BulkMeta => rsx! { BulkMetaView {} },
            }
        }
    }
}

/// The grounded queue + a verify form for the selected rule.
#[component]
fn QueueView() -> Element {
    // Load the grounded queue asynchronously via the CORE.
    let queue = use_resource(|| async move { list_grounded(&corpus_dir()).await });
    let selected = use_signal(|| Option::<GroundedRow>::None);

    rsx! {
        div {
            style: "display:flex; gap:24px;",
            // Left: the queue list.
            div {
                style: "flex: 1; min-width: 320px;",
                h2 { "Grounded queue" }
                match &*queue.read_unchecked() {
                    Some(Ok(rows)) if rows.is_empty() => rsx! { p { "No grounded rules. Nothing to verify." } },
                    Some(Ok(rows)) => rsx! {
                        ul {
                            style: "list-style:none; padding:0;",
                            for row in rows.iter().cloned() {
                                {
                                    let r = row.clone();
                                    let mut sel = selected;
                                    rsx! {
                                        li {
                                            key: "{row.id}",
                                            style: "padding:6px 8px; border-bottom:1px solid #eee; cursor:pointer;",
                                            onclick: move |_| sel.set(Some(r.clone())),
                                            strong { "{row.id}" }
                                            span { style:"color:#888;", "  [{row.domain} / {row.enforcement}]" }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    Some(Err(e)) => rsx! { p { style:"color:#b00;", "Failed to load corpus: {e}" } },
                    None => rsx! { p { "Loading corpus..." } },
                }
            }
            // Right: the verify form for the selected rule.
            div {
                style: "flex: 1; min-width: 360px;",
                match selected() {
                    Some(row) => rsx! { VerifyForm { row } },
                    None => rsx! { p { style:"color:#888;", "Pick a rule from the queue to verify." } },
                }
            }
        }
    }
}

/// The prefilled verify form for one rule.
#[component]
fn VerifyForm(row: GroundedRow) -> Element {
    // `by` prefilled from $USER; `at` prefilled with today; `against` editable,
    // prefilled with the rule's primary source as a starting anchor.
    let by = use_signal(|| std::env::var("USER").unwrap_or_else(|_| "maintainer".to_owned()));
    let at = use_signal(today);
    let against = use_signal(|| row.primary_source.clone().unwrap_or_default());
    let mut result = use_signal(|| Option::<Result<String, String>>::None);
    // Preview-first: dry-run is ON by default; uncheck to fire the real branch -> PR.
    let dry_run = use_signal(|| true);

    // The inline EXAMPLE of the [verified] block that will be written.
    let against_lines: Vec<String> = against()
        .lines()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let preview = verified_block_preview(&by(), &at(), &against_lines);

    let rule_id = row.id.clone();
    let on_verify = move |_| {
        let rule_id = rule_id.clone();
        let by_v = by();
        let at_v = at();
        let against_v = against_lines.clone();
        let dry_v = dry_run();
        spawn(async move {
            // Dry-run edits the TOML locally and reports the planned PR without
            // pushing; unchecking it fires the real branch -> push -> PR via GitVcs.
            let outcome = if dry_v {
                verify_one(&corpus_dir(), &rule_id, &by_v, &at_v, against_v, &DryRunVcs::new()).await
            } else {
                verify_one(&corpus_dir(), &rule_id, &by_v, &at_v, against_v, &GitVcs::new()).await
            };
            result.set(Some(match outcome {
                Ok(o) => Ok(if dry_v {
                    format!("Planned PR (dry-run): {}", o.pr_url)
                } else {
                    format!("PR opened: {}", o.pr_url)
                }),
                Err(e) => Err(e.to_string()),
            }));
        });
    };

    rsx! {
        h2 { "Verify {row.id}" }
        p { style:"color:#888;", "{row.domain} / {row.enforcement}" }

        label { "Verified by" }
        input {
            style: "display:block; width:100%; margin:4px 0 12px;",
            value: "{by}",
            oninput: move |e| by.clone().set(e.value()),
        }
        label { "Date (at)" }
        input {
            style: "display:block; width:100%; margin:4px 0 12px;",
            value: "{at}",
            oninput: move |e| at.clone().set(e.value()),
        }
        label { "Against (one anchor per line)" }
        textarea {
            style: "display:block; width:100%; height:80px; margin:4px 0 12px;",
            value: "{against}",
            oninput: move |e| against.clone().set(e.value()),
        }

        h3 { "Resulting [verified] block" }
        pre {
            style: "background:#f6f6f6; padding:10px; border-radius:6px; white-space:pre-wrap;",
            "{preview}"
        }

        label { style:"display:block; margin-top:8px;",
            input {
                r#type: "checkbox",
                checked: dry_run(),
                onchange: move |e| dry_run.clone().set(e.checked()),
            }
            " Dry run (preview only — uncheck to open a real PR into main)"
        }
        button {
            style: "margin-top:8px; padding:8px 14px;",
            onclick: on_verify,
            "Verify rule"
        }

        match result() {
            Some(Ok(url)) => rsx! {
                p { style:"color:#070; margin-top:12px;", "{url}" }
            },
            Some(Err(e)) => rsx! {
                p { style:"color:#b00; margin-top:12px;", "Error: {e}" }
            },
            None => rsx! {},
        }
    }
}

/// The bulk self-source view for the maintainer-authored meta rules.
#[component]
fn BulkMetaView() -> Element {
    let by = use_signal(|| std::env::var("USER").unwrap_or_else(|_| "maintainer".to_owned()));
    let domain = use_signal(String::new); // empty => all meta domains
    let mut result = use_signal(|| Option::<Result<String, String>>::None);
    let dry_run = use_signal(|| true);

    let on_run = move |_| {
        let by_v = by();
        let dom = domain();
        let dry_v = dry_run();
        spawn(async move {
            let scope = if dom.trim().is_empty() {
                None
            } else {
                Some(dom.trim().to_owned())
            };
            let outcome = if dry_v {
                self_source(&corpus_dir(), scope.as_deref(), &by_v, &today(), &DryRunVcs::new()).await
            } else {
                self_source(&corpus_dir(), scope.as_deref(), &by_v, &today(), &GitVcs::new()).await
            };
            result.set(Some(match outcome {
                Ok(o) => Ok(if dry_v {
                    format!("Planned PR (dry-run): {}", o.pr_url)
                } else {
                    format!("PR opened: {}", o.pr_url)
                }),
                Err(e) => Err(e.to_string()),
            }));
        });
    };

    rsx! {
        h2 { "Bulk self-source meta rules" }
        p {
            style:"color:#666;",
            "Flips every grounded maintainer-authored meta rule to verified "
            "(against = self-sourced), batched into ONE branch + ONE PR. "
            "Leave domain blank to cover all meta domains."
        }
        label { "Verified by" }
        input {
            style: "display:block; width:100%; margin:4px 0 12px;",
            value: "{by}",
            oninput: move |e| by.clone().set(e.value()),
        }
        label { "Meta domain (blank = all: agentic, api-layer, ui, permissions, universal)" }
        input {
            style: "display:block; width:100%; margin:4px 0 12px;",
            value: "{domain}",
            oninput: move |e| domain.clone().set(e.value()),
        }
        label { style:"display:block; margin:8px 0;",
            input {
                r#type: "checkbox",
                checked: dry_run(),
                onchange: move |e| dry_run.clone().set(e.checked()),
            }
            " Dry run (preview only — uncheck to open a real PR into main)"
        }
        button {
            style: "padding:8px 14px;",
            onclick: on_run,
            "Bulk self-source"
        }
        match result() {
            Some(Ok(url)) => rsx! {
                p { style:"color:#070; margin-top:12px;", "{url}" }
            },
            Some(Err(e)) => rsx! {
                p { style:"color:#b00; margin-top:12px;", "Error: {e}" }
            },
            None => rsx! {},
        }
    }
}
