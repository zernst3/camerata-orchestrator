//! The corpus-freshness line of the cockpit's update banner (design:
//! `docs/design/2026-10-20_corpus-staleness-check.md`, "Amendment 2026-10-21", part 4).
//!
//! `GET /api/updates/check` carries a `corpus_freshness` object (`overdue`, `due_soon`,
//! `never_confirmed_count`, or `null` when freshness is unknown). This module turns it into at
//! most one line for the operator:
//!
//! - Anything overdue or due soon is an ALERT line: it names the packs and the days, because it
//!   needs action.
//! - "Never confirmed" alone is a QUIET note. Before the first sweep that is the honest state of
//!   every pack, so it must not shout; it also lists no names, only a count.
//! - Nothing to say (or unknown) renders nothing.
//!
//! The text is operator-only. The cockpit is the operator's surface; none of this reaches a
//! client artifact (a gate in `camerata-server` enforces it).

use dioxus::prelude::*;

/// One overdue pack, as the server sends it. Only the fields the line shows.
#[derive(Clone, PartialEq, Debug, serde::Deserialize)]
pub(super) struct OverduePackView {
    pub domain: String,
    #[serde(default)]
    pub days_since: i64,
}

/// One pack whose window lapses soon.
#[derive(Clone, PartialEq, Debug, serde::Deserialize)]
pub(super) struct DueSoonPackView {
    pub domain: String,
    #[serde(default)]
    pub days_remaining: i64,
}

/// The `corpus_freshness` object of `GET /api/updates/check`.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize)]
pub(super) struct CorpusFreshnessView {
    #[serde(default)]
    pub overdue: Vec<OverduePackView>,
    #[serde(default)]
    pub due_soon: Vec<DueSoonPackView>,
    #[serde(default)]
    pub never_confirmed_count: usize,
}

/// How loud the line is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FreshnessTone {
    /// Something is overdue or due soon: needs action.
    Alert,
    /// Only "never confirmed": informational, the pre-first-sweep state.
    Quiet,
}

/// The single line to show, or nothing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct FreshnessLine {
    pub tone: FreshnessTone,
    pub text: String,
}

fn days(n: i64) -> String {
    match n {
        1 => "1 day".to_string(),
        n => format!("{n} days"),
    }
}

/// The line for `view`, or `None` when there is nothing to show. Pure.
pub(super) fn corpus_freshness_line(view: &CorpusFreshnessView) -> Option<FreshnessLine> {
    if !view.overdue.is_empty() || !view.due_soon.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if !view.overdue.is_empty() {
            let packs: Vec<String> = view
                .overdue
                .iter()
                .map(|o| format!("{} ({})", o.domain, days(o.days_since)))
                .collect();
            parts.push(format!(
                "overdue for a staleness sweep: {}",
                packs.join(", ")
            ));
        }
        if !view.due_soon.is_empty() {
            let packs: Vec<String> = view
                .due_soon
                .iter()
                .map(|d| match d.days_remaining {
                    0 => format!("{} today", d.domain),
                    n => format!("{} in {}", d.domain, days(n)),
                })
                .collect();
            parts.push(format!("due soon: {}", packs.join(", ")));
        }
        return Some(FreshnessLine {
            tone: FreshnessTone::Alert,
            text: format!("Corpus freshness: {}.", parts.join("; ")),
        });
    }
    match view.never_confirmed_count {
        0 => None,
        1 => Some(FreshnessLine {
            tone: FreshnessTone::Quiet,
            text: "Corpus freshness: 1 rule pack has not been confirmed by a sweep yet."
                .to_string(),
        }),
        n => Some(FreshnessLine {
            tone: FreshnessTone::Quiet,
            text: format!(
                "Corpus freshness: {n} rule packs have not been confirmed by a sweep yet."
            ),
        }),
    }
}

/// The freshness line, dismissible for the session (not persisted), or nothing. An alert reuses
/// the update banner's strip so it reads as the same family; the quiet note is small muted text.
#[component]
pub(super) fn CorpusFreshnessLine(view: CorpusFreshnessView) -> Element {
    let mut dismissed = use_signal(|| false);
    let Some(line) = corpus_freshness_line(&view) else {
        return rsx! {};
    };
    if dismissed() {
        return rsx! {};
    }
    match line.tone {
        FreshnessTone::Alert => rsx! {
            div { class: "app-update-banner corpus-freshness-alert",
                span { class: "app-update-icon", "\u{26A0}" }
                span { class: "app-update-text", "{line.text}" }
                button {
                    class: "app-update-dismiss",
                    onclick: move |_| dismissed.set(true),
                    "\u{00D7}"
                }
            }
        },
        FreshnessTone::Quiet => rsx! {
            div { class: "corpus-freshness-note",
                span { "{line.text}" }
                button {
                    class: "app-update-dismiss",
                    onclick: move |_| dismissed.set(true),
                    "\u{00D7}"
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(overdue: &[(&str, i64)], soon: &[(&str, i64)], never: usize) -> CorpusFreshnessView {
        CorpusFreshnessView {
            overdue: overdue
                .iter()
                .map(|(d, n)| OverduePackView {
                    domain: d.to_string(),
                    days_since: *n,
                })
                .collect(),
            due_soon: soon
                .iter()
                .map(|(d, n)| DueSoonPackView {
                    domain: d.to_string(),
                    days_remaining: *n,
                })
                .collect(),
            never_confirmed_count: never,
        }
    }

    #[test]
    fn deserializes_the_server_shape_and_tolerates_missing_keys() {
        let v: CorpusFreshnessView = serde_json::from_value(serde_json::json!({
            "overdue": [{
                "domain": "go", "last_confirmed": "2026-01-01",
                "days_since": 111, "threshold_days": 90
            }],
            "due_soon": [{
                "domain": "rust", "last_confirmed": "2026-07-25", "days_since": 87,
                "threshold_days": 90, "days_remaining": 3
            }],
            "never_confirmed_count": 4
        }))
        .expect("server shape");
        assert_eq!(v.overdue[0].domain, "go");
        assert_eq!(v.overdue[0].days_since, 111);
        assert_eq!(v.due_soon[0].days_remaining, 3);
        assert_eq!(v.never_confirmed_count, 4);

        let empty: CorpusFreshnessView =
            serde_json::from_value(serde_json::json!({})).expect("all keys optional");
        assert_eq!(empty, CorpusFreshnessView::default());
    }

    #[test]
    fn nothing_to_say_renders_no_line() {
        assert_eq!(corpus_freshness_line(&view(&[], &[], 0)), None);
    }

    #[test]
    fn overdue_is_a_loud_line_naming_the_pack_and_days() {
        let line = corpus_freshness_line(&view(&[("go", 111)], &[], 0)).expect("line");
        assert_eq!(line.tone, FreshnessTone::Alert);
        assert_eq!(
            line.text,
            "Corpus freshness: overdue for a staleness sweep: go (111 days)."
        );
    }

    #[test]
    fn due_soon_is_a_loud_line_with_days_remaining() {
        let line = corpus_freshness_line(&view(&[], &[("rust", 3), ("sql", 1), ("go", 0)], 0))
            .expect("line");
        assert_eq!(line.tone, FreshnessTone::Alert);
        assert_eq!(
            line.text,
            "Corpus freshness: due soon: rust in 3 days, sql in 1 day, go today."
        );
    }

    #[test]
    fn overdue_and_due_soon_share_one_line() {
        let line = corpus_freshness_line(&view(&[("go", 111)], &[("rust", 3)], 5)).expect("line");
        assert_eq!(line.tone, FreshnessTone::Alert);
        assert_eq!(
            line.text,
            "Corpus freshness: overdue for a staleness sweep: go (111 days); due soon: rust in 3 days."
        );
        // The never-confirmed count rides quietly behind an alert; it is not repeated in it.
        assert!(!line.text.contains("not been confirmed"));
    }

    #[test]
    fn never_confirmed_alone_is_a_quiet_count_only_note() {
        let one = corpus_freshness_line(&view(&[], &[], 1)).expect("line");
        assert_eq!(one.tone, FreshnessTone::Quiet);
        assert_eq!(
            one.text,
            "Corpus freshness: 1 rule pack has not been confirmed by a sweep yet."
        );
        let many = corpus_freshness_line(&view(&[], &[], 12)).expect("line");
        assert_eq!(many.tone, FreshnessTone::Quiet);
        assert!(many.text.contains("12 rule packs"), "{}", many.text);
        // No domain names, no urgency vocabulary: it is the honest pre-first-sweep state.
        assert!(!many.text.to_lowercase().contains("overdue"));
    }

    #[test]
    fn the_line_never_uses_an_em_dash() {
        for v in [view(&[("go", 111)], &[("rust", 3)], 2), view(&[], &[], 3)] {
            let text = corpus_freshness_line(&v).expect("line").text;
            assert!(!text.contains('\u{2014}'), "{text}");
        }
    }

    // ── rendering ───────────────────────────────────────────────────────────

    fn render(v: CorpusFreshnessView) -> String {
        let mut vdom =
            VirtualDom::new_with_props(CorpusFreshnessLine, CorpusFreshnessLineProps { view: v });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    #[test]
    fn an_alert_renders_in_the_update_banner_strip() {
        let html = render(view(&[("go", 111)], &[], 0));
        assert!(html.contains("app-update-banner"), "{html}");
        assert!(html.contains("corpus-freshness-alert"), "{html}");
        assert!(html.contains("go (111 days)"), "{html}");
        assert!(!html.contains("corpus-freshness-note"), "{html}");
    }

    #[test]
    fn a_never_confirmed_note_renders_quietly_not_as_a_banner() {
        let html = render(view(&[], &[], 4));
        assert!(html.contains("corpus-freshness-note"), "{html}");
        assert!(
            !html.contains("app-update-banner"),
            "the quiet note must not use the alert strip: {html}"
        );
        assert!(html.contains("4 rule packs"), "{html}");
    }

    #[test]
    fn nothing_to_say_renders_nothing_at_all() {
        let html = render(view(&[], &[], 0));
        assert!(!html.contains("corpus-freshness"), "{html}");
        assert!(!html.contains("app-update"), "{html}");
    }
}
