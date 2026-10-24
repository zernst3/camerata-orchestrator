//! Client source lifecycle: the pure logic behind the Workspace repo card, the "Delete source
//! code from this machine" confirmation and the re-fetch dialog (docs/design/2026-10-24_delete-
//! and-refetch-client-source.md, section 6). No rendering framework (RUST-HEADLESS-CORE-1):
//! everything here is unit-tested without a VirtualDom, and the adapter only supplies words and
//! pixels.

use serde::Deserialize;

/// The state of one repo's source on this machine, as `GET /api/projects/:id/source` and the
/// checkout list report it (`state` is the tag). Mirrors the server's `RepoSourceStatus`; fields
/// the card does not use are still parsed so the wire contract is documented in one place.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SourceView {
    /// A matching clone is on disk.
    Present {
        repo: String,
        path: String,
        #[serde(default)]
        head_sha: Option<String>,
        #[serde(default)]
        head_branch: Option<String>,
        #[serde(default)]
        dirty: bool,
        #[serde(default)]
        inspected_sha: Option<String>,
        #[serde(default)]
        inspected_at: Option<String>,
        /// Commits between the inspected commit and HEAD; `None` with no inspection or when the
        /// inspected commit is not in the clone.
        #[serde(default)]
        newer_commits: Option<u64>,
        #[serde(default)]
        inspected_commit_reachable: bool,
        #[serde(default)]
        has_deletion_history: bool,
    },
    /// The source was deleted by Camerata and has not been fetched back.
    Deleted {
        repo: String,
        path: String,
        deleted_at: String,
        record_id: String,
        #[serde(default)]
        inspected_sha: Option<String>,
        #[serde(default)]
        total_files: u64,
        #[serde(default)]
        total_bytes: u64,
        /// The server's one-line account of what was removed ("2,729 files, 182.9 MB removed").
        #[serde(default)]
        summary: String,
    },
    /// Nothing on disk and no deletion on record: the pre-existing "not cloned / not linked".
    Unresolved {
        repo: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        reason: String,
    },
    /// A folder is there but is not this repository.
    Mismatch {
        repo: String,
        #[serde(default)]
        path: String,
        #[serde(default)]
        reason: String,
    },
}

/// The four states of the Workspace repo card, refined by how the source relates to the
/// inspection (design 6.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardState {
    /// Source on disk at exactly the inspected commit.
    AtInspected { sha: String },
    /// Source on disk, `commits` newer than the inspected commit.
    Newer {
        sha: String,
        inspected: String,
        commits: u64,
    },
    /// Source on disk, but the inspected commit is not in this clone (history was rewritten).
    InspectedUnreachable { sha: String, inspected: String },
    /// Source on disk and nothing to compare it with (no inspection has run for it).
    PresentUncompared { sha: Option<String> },
    /// Source deleted on purpose.
    Deleted {
        deleted_at: String,
        summary: String,
        record_id: String,
    },
    /// Not cloned / not linked (today's "Not cloned" card).
    Unresolved { reason: String },
    /// A folder is there but fails the guard.
    Mismatch { reason: String },
}

/// First seven characters of a commit id.
pub fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Map the server's source status to the card state. Pure.
pub fn card_state(view: &SourceView) -> CardState {
    match view {
        SourceView::Present {
            head_sha,
            inspected_sha,
            newer_commits,
            inspected_commit_reachable,
            ..
        } => {
            let head = head_sha.clone();
            match (inspected_sha, head) {
                (Some(inspected), Some(head)) if *inspected_commit_reachable => {
                    match *newer_commits {
                        Some(0) => CardState::AtInspected { sha: head },
                        Some(commits) => CardState::Newer {
                            sha: head,
                            inspected: inspected.clone(),
                            commits,
                        },
                        None => CardState::PresentUncompared { sha: Some(head) },
                    }
                }
                (Some(inspected), Some(head)) => CardState::InspectedUnreachable {
                    sha: head,
                    inspected: inspected.clone(),
                },
                (_, head) => CardState::PresentUncompared { sha: head },
            }
        }
        SourceView::Deleted {
            deleted_at,
            summary,
            record_id,
            ..
        } => CardState::Deleted {
            deleted_at: deleted_at.clone(),
            summary: summary.clone(),
            record_id: record_id.clone(),
        },
        SourceView::Unresolved { reason, .. } => CardState::Unresolved {
            reason: reason.clone(),
        },
        SourceView::Mismatch { reason, .. } => CardState::Mismatch {
            reason: reason.clone(),
        },
    }
}

/// The badge sentence of a card state.
pub fn badge_text(state: &CardState) -> String {
    match state {
        CardState::AtInspected { sha } => {
            format!("Source at {} (the inspected commit)", short_sha(sha))
        }
        CardState::Newer {
            sha,
            inspected,
            commits,
        } => format!(
            "Source at {}, {commits} {} newer than the inspection ({})",
            short_sha(sha),
            if *commits == 1 { "commit" } else { "commits" },
            short_sha(inspected)
        ),
        CardState::InspectedUnreachable { sha, inspected } => format!(
            "Source at {}; the inspected commit {} is not in this clone (history was \
             rewritten). Finding code views for the inspection are unavailable.",
            short_sha(sha),
            short_sha(inspected)
        ),
        CardState::PresentUncompared { sha } => match sha {
            Some(sha) => format!("Source at {}", short_sha(sha)),
            None => "Source on this machine".to_string(),
        },
        CardState::Deleted { deleted_at, .. } => {
            format!("Source deleted on {}", format_date(deleted_at))
        }
        CardState::Unresolved { reason } | CardState::Mismatch { reason } => reason.clone(),
    }
}

/// Whether the card offers the destructive "Delete source code from this machine" (any state
/// where source is on disk and matches the repo).
pub fn offers_delete(state: &CardState) -> bool {
    matches!(
        state,
        CardState::AtInspected { .. }
            | CardState::Newer { .. }
            | CardState::InspectedUnreachable { .. }
            | CardState::PresentUncompared { .. }
    )
}

/// Whether the card offers "Rescan changes" (the delta retest): source on disk, newer than the
/// inspection, with the inspected commit still in the clone.
pub fn offers_rescan_changes(state: &CardState) -> bool {
    matches!(state, CardState::Newer { .. })
}

/// Whether the card offers "Re-fetch source": the source was deleted, or was never here.
pub fn offers_refetch(state: &CardState) -> bool {
    matches!(state, CardState::Deleted { .. })
}

/// Whether the delete button in the confirmation modal is enabled: the operator typed the
/// repository's name EXACTLY (byte for byte; no trimming, no case folding, paste allowed) and,
/// when the plan warns of uncommitted or unpushed work, ticked "Delete uncommitted work too".
pub fn confirm_enabled(typed: &str, repo: &str, warnings_present: bool, ticked: bool) -> bool {
    typed == repo && (!warnings_present || ticked)
}

/// Which code the re-fetch brings back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefetchChoice {
    /// The default branch's head (the code remediation and the monthly retest need).
    Latest,
    /// Exactly the commit the inspection read (to reproduce or dispute a finding).
    Inspected,
}

/// The two radio options of the re-fetch dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefetchOptions {
    pub latest_label: String,
    /// `None` when no inspected commit is known (the option is then not offered).
    pub inspected_label: Option<String>,
    pub inspected_hint: &'static str,
    pub default: RefetchChoice,
}

/// Build the dialog's options from what the remote probe and the inspection report. The latest
/// source is always pre-selected; when the remote head IS the inspected commit the label says so.
pub fn refetch_options(
    default_branch: Option<&str>,
    remote_head: &str,
    inspected_sha: Option<&str>,
    inspected_at: Option<&str>,
) -> RefetchOptions {
    let branch = default_branch.unwrap_or("the default branch");
    let head = short_sha(remote_head);
    let latest_label = if inspected_sha.is_some_and(|s| s == remote_head) {
        format!("Latest from {branch} ({head}, the inspected commit)")
    } else {
        format!("Latest from {branch} ({head})")
    };
    let inspected_label = inspected_sha.map(|sha| match inspected_at {
        Some(at) => format!(
            "The inspected commit {}, {}",
            short_sha(sha),
            format_date(at)
        ),
        None => format!("The inspected commit {}", short_sha(sha)),
    });
    RefetchOptions {
        latest_label,
        inspected_label,
        inspected_hint: "For reproducing or disputing a finding. The clone still has full history.",
        default: RefetchChoice::Latest,
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The scan surface's header for a delta retest (design 8): `Delta retest of changes since
/// 3f2a9c1 (14 commits, 6 files changed)`. The commit count is left out when unknown.
pub fn delta_header(from_sha: &str, commits: Option<u64>, changed_files: u64) -> String {
    let files = plural(changed_files, "file changed", "files changed");
    let detail = match commits {
        Some(c) => format!("{}, {files}", plural(c, "commit", "commits")),
        None => files,
    };
    format!(
        "Delta retest of changes since {} ({detail})",
        short_sha(from_sha)
    )
}

/// The note the finding modal shows about WHERE its code block came from (design 6.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextNote {
    /// The source is deleted; the block area shows this instead of code. The stored snippet is
    /// still shown above it.
    SourceDeleted(String),
    /// The block is from the inspected commit and the checkout has moved on: muted note.
    NewerSource(String),
    /// The inspected commit is not in the clone; the working tree is shown instead: amber banner.
    CommitUnavailable(String),
}

/// Which note, if any, goes with a `GET /api/onboard/finding-context` answer. `source` is the
/// response's `source` field (`"git"` or `"working_tree"`, empty when absent), `commit` its
/// `commit`. Pure.
pub fn context_note(
    status: &str,
    source: &str,
    commit: &str,
    head_differs: bool,
    deleted_at: Option<&str>,
) -> Option<ContextNote> {
    match status {
        "source_deleted" => Some(ContextNote::SourceDeleted(match deleted_at {
            Some(at) if !at.is_empty() => {
                format!("Source deleted on {}, re-fetch to view", format_date(at))
            }
            _ => "Source deleted, re-fetch to view".to_string(),
        })),
        "ok" if source == "git" && head_differs && !commit.is_empty() => {
            Some(ContextNote::NewerSource(format!(
                "References commit {}; source on disk is newer",
                short_sha(commit)
            )))
        }
        "commit_unavailable" => Some(ContextNote::CommitUnavailable(
            "Inspected commit not available in this clone; showing the working tree".to_string(),
        )),
        _ => None,
    }
}

/// `24 Oct 2026` from an RFC3339 timestamp (`2026-10-24T17:40:19Z`). Only the date part is read,
/// so any offset or fractional seconds are ignored; anything that does not start with a valid
/// `YYYY-MM-DD` is returned unchanged rather than guessed at.
pub fn format_date(rfc3339: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let date = rfc3339.get(..10).unwrap_or("");
    let mut parts = date.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return rfc3339.to_string();
    };
    let (Ok(year), Ok(month), Ok(day)) = (y.parse::<u32>(), m.parse::<usize>(), d.parse::<u32>())
    else {
        return rfc3339.to_string();
    };
    if y.len() != 4 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return rfc3339.to_string();
    }
    format!("{day} {} {year}", MONTHS[month - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(
        head: &str,
        inspected: Option<&str>,
        newer: Option<u64>,
        reachable: bool,
    ) -> SourceView {
        SourceView::Present {
            repo: "acme/app".to_string(),
            path: "/work/acme/app".to_string(),
            head_sha: Some(head.to_string()),
            head_branch: Some("main".to_string()),
            dirty: false,
            inspected_sha: inspected.map(str::to_string),
            inspected_at: inspected.map(|_| "2026-10-24T10:05:00Z".to_string()),
            newer_commits: newer,
            inspected_commit_reachable: reachable,
            has_deletion_history: false,
        }
    }

    const A: &str = "3f2a9c14e0b7aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "9b7e21c0000bbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn dates_read_the_way_the_card_shows_them() {
        assert_eq!(format_date("2026-10-24T17:40:19Z"), "24 Oct 2026");
        assert_eq!(format_date("2026-01-05T00:00:00+02:00"), "5 Jan 2026");
        assert_eq!(format_date("2026-12-31"), "31 Dec 2026");
    }

    #[test]
    fn anything_that_is_not_a_date_is_returned_as_given() {
        assert_eq!(format_date(""), "");
        assert_eq!(format_date("yesterday"), "yesterday");
        assert_eq!(format_date("2026-13-01T00:00:00Z"), "2026-13-01T00:00:00Z");
        assert_eq!(format_date("26-10-24T00:00:00Z"), "26-10-24T00:00:00Z");
    }

    #[test]
    fn card_state_maps_each_source_status() {
        // At the inspected commit.
        let s = card_state(&present(A, Some(A), Some(0), true));
        assert_eq!(s, CardState::AtInspected { sha: A.to_string() });
        assert_eq!(badge_text(&s), "Source at 3f2a9c1 (the inspected commit)");

        // Newer.
        let s = card_state(&present(B, Some(A), Some(14), true));
        assert_eq!(
            s,
            CardState::Newer {
                sha: B.to_string(),
                inspected: A.to_string(),
                commits: 14
            }
        );
        assert_eq!(
            badge_text(&s),
            "Source at 9b7e21c, 14 commits newer than the inspection (3f2a9c1)"
        );
        assert!(
            badge_text(&card_state(&present(B, Some(A), Some(1), true))).contains("1 commit newer")
        );

        // Inspected commit not in this clone.
        let s = card_state(&present(B, Some(A), None, false));
        assert_eq!(
            s,
            CardState::InspectedUnreachable {
                sha: B.to_string(),
                inspected: A.to_string()
            }
        );
        assert!(badge_text(&s).contains("not in this clone"));
        assert!(badge_text(&s).contains("history was rewritten"));

        // No inspection to compare with.
        let s = card_state(&present(B, None, None, false));
        assert_eq!(
            s,
            CardState::PresentUncompared {
                sha: Some(B.to_string())
            }
        );
        assert_eq!(badge_text(&s), "Source at 9b7e21c");

        // Deleted.
        let deleted = SourceView::Deleted {
            repo: "acme/app".to_string(),
            path: "/work/acme/app".to_string(),
            deleted_at: "2026-10-24T17:40:19Z".to_string(),
            record_id: "del-p-acme-app-20261024174019".to_string(),
            inspected_sha: Some(A.to_string()),
            total_files: 2729,
            total_bytes: 182_900_000,
            summary: "2,729 files, 182.9 MB removed".to_string(),
        };
        let s = card_state(&deleted);
        assert_eq!(badge_text(&s), "Source deleted on 24 Oct 2026");
        assert!(
            matches!(&s, CardState::Deleted { summary, .. } if summary == "2,729 files, 182.9 MB removed")
        );

        // Unresolved and mismatch keep the server's reason.
        let s = card_state(&SourceView::Unresolved {
            repo: "acme/app".to_string(),
            path: None,
            reason: "not cloned yet".to_string(),
        });
        assert_eq!(badge_text(&s), "not cloned yet");
        let s = card_state(&SourceView::Mismatch {
            repo: "acme/app".to_string(),
            path: "/x".to_string(),
            reason: "that folder is a different repo (origin: acme/other)".to_string(),
        });
        assert!(badge_text(&s).contains("acme/other"));
    }

    #[test]
    fn which_actions_each_card_state_offers() {
        let at = card_state(&present(A, Some(A), Some(0), true));
        let newer = card_state(&present(B, Some(A), Some(3), true));
        let unreachable = card_state(&present(B, Some(A), None, false));
        let bare = card_state(&present(B, None, None, false));
        let deleted = CardState::Deleted {
            deleted_at: "2026-10-24T17:40:19Z".to_string(),
            summary: String::new(),
            record_id: "r".to_string(),
        };
        let missing = CardState::Unresolved {
            reason: "not cloned yet".to_string(),
        };

        for s in [&at, &newer, &unreachable, &bare] {
            assert!(offers_delete(s), "{s:?}");
            assert!(!offers_refetch(s), "{s:?}");
        }
        assert!(offers_rescan_changes(&newer));
        for s in [&at, &unreachable, &bare, &deleted, &missing] {
            assert!(
                !offers_rescan_changes(s),
                "only a newer clone is rescanned: {s:?}"
            );
        }
        assert!(offers_refetch(&deleted));
        assert!(!offers_delete(&deleted) && !offers_delete(&missing));
        assert!(
            !offers_refetch(&missing),
            "an unlinked repo links; it does not re-fetch"
        );
    }

    #[test]
    fn short_shas_never_panic_on_short_input() {
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha(""), "");
        assert_eq!(short_sha(A), "3f2a9c1");
    }

    #[test]
    fn confirm_enabled_requires_exact_name_and_tick_when_warned() {
        // Exact name, nothing to warn about.
        assert!(confirm_enabled("acme/app", "acme/app", false, false));
        // Anything but the exact bytes keeps the button off.
        for typed in [
            "",
            "acme/ap",
            "ACME/APP",
            "Acme/App",
            " acme/app",
            "acme/app ",
            "acme/app\n",
        ] {
            assert!(
                !confirm_enabled(typed, "acme/app", false, true),
                "{typed:?}"
            );
        }
        // Warnings (uncommitted or unpushed work) need the tick as well.
        assert!(!confirm_enabled("acme/app", "acme/app", true, false));
        assert!(confirm_enabled("acme/app", "acme/app", true, true));
        // The tick alone is never enough.
        assert!(!confirm_enabled("nope", "acme/app", true, true));
    }

    #[test]
    fn refetch_dialog_default_is_latest_and_labels_inspected_commit_when_equal() {
        // Remote has moved on: two distinct options, latest pre-selected.
        let o = refetch_options(Some("main"), B, Some(A), Some("2026-10-24T10:05:00Z"));
        assert_eq!(o.default, RefetchChoice::Latest);
        assert_eq!(o.latest_label, "Latest from main (9b7e21c)");
        assert_eq!(
            o.inspected_label.as_deref(),
            Some("The inspected commit 3f2a9c1, 24 Oct 2026")
        );
        assert!(o.inspected_hint.contains("full history"));

        // Remote head IS the inspected commit: the latest option says so.
        let o = refetch_options(Some("main"), A, Some(A), Some("2026-10-24T10:05:00Z"));
        assert_eq!(
            o.latest_label,
            "Latest from main (3f2a9c1, the inspected commit)"
        );
        assert_eq!(o.default, RefetchChoice::Latest);

        // No inspection: only the latest option exists. No default branch is still readable.
        let o = refetch_options(None, B, None, None);
        assert_eq!(o.inspected_label, None);
        assert_eq!(o.latest_label, "Latest from the default branch (9b7e21c)");
        // An inspected commit without a date still has a label.
        let o = refetch_options(Some("main"), B, Some(A), None);
        assert_eq!(
            o.inspected_label.as_deref(),
            Some("The inspected commit 3f2a9c1")
        );
    }

    #[test]
    fn delta_header_names_the_base_commit_and_the_size_of_the_change() {
        assert_eq!(
            delta_header(A, Some(14), 6),
            "Delta retest of changes since 3f2a9c1 (14 commits, 6 files changed)"
        );
        assert_eq!(
            delta_header(A, Some(1), 1),
            "Delta retest of changes since 3f2a9c1 (1 commit, 1 file changed)"
        );
        assert_eq!(
            delta_header(A, None, 2),
            "Delta retest of changes since 3f2a9c1 (2 files changed)"
        );
    }

    #[test]
    fn context_note_covers_the_three_banners_and_stays_quiet_otherwise() {
        assert_eq!(
            context_note(
                "source_deleted",
                "",
                "",
                false,
                Some("2026-10-24T17:40:19Z")
            ),
            Some(ContextNote::SourceDeleted(
                "Source deleted on 24 Oct 2026, re-fetch to view".to_string()
            ))
        );
        assert_eq!(
            context_note("source_deleted", "", "", false, None),
            Some(ContextNote::SourceDeleted(
                "Source deleted, re-fetch to view".to_string()
            ))
        );
        assert_eq!(
            context_note("ok", "git", A, true, None),
            Some(ContextNote::NewerSource(
                "References commit 3f2a9c1; source on disk is newer".to_string()
            ))
        );
        assert_eq!(
            context_note("commit_unavailable", "working_tree", "", false, None),
            Some(ContextNote::CommitUnavailable(
                "Inspected commit not available in this clone; showing the working tree"
                    .to_string()
            ))
        );
        // At the inspected commit, from the working tree, or degraded: no note.
        assert_eq!(context_note("ok", "git", A, false, None), None);
        assert_eq!(context_note("ok", "working_tree", "", true, None), None);
        assert_eq!(context_note("file_missing", "", "", false, None), None);
    }

    #[test]
    fn the_wire_status_deserializes_into_the_view() {
        let json = r#"{"state":"present","repo":"acme/app","path":"/w","head_sha":"abc","head_branch":"main","dirty":false,"inspected_sha":"def","inspected_at":"2026-10-24T10:05:00Z","newer_commits":3,"inspected_commit_reachable":true,"has_deletion_history":true}"#;
        let v: SourceView = serde_json::from_str(json).unwrap();
        assert!(matches!(
            v,
            SourceView::Present {
                newer_commits: Some(3),
                has_deletion_history: true,
                ..
            }
        ));
        let json = r#"{"state":"deleted","repo":"acme/app","path":"/w","deleted_at":"2026-10-24T17:40:19Z","record_id":"del-1","inspected_sha":null,"total_files":12,"total_bytes":3400}"#;
        let v: SourceView = serde_json::from_str(json).unwrap();
        assert!(matches!(
            v,
            SourceView::Deleted {
                total_files: 12,
                ..
            }
        ));
        let json =
            r#"{"state":"unresolved","repo":"acme/app","path":null,"reason":"not cloned yet"}"#;
        assert!(matches!(
            serde_json::from_str::<SourceView>(json).unwrap(),
            SourceView::Unresolved { path: None, .. }
        ));
    }
}
