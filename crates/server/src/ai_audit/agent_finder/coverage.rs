//! Coverage of a piece from its tool trace only (design section 4, MVP checklist item 4: the
//! model's own list never changes a state).

use std::collections::BTreeMap;

use super::room::NotServed;
use super::{AgentRun, ToolEvent};

/// What happened to one file of the piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// The lines returned across all reads cover the whole file.
    ReadInFull,
    /// Some lines were read: the ranges `(first, last)`, 1-based and inclusive, merged.
    ReadInPart(Vec<(usize, usize)>),
    /// Shown to the model in full by the send-again step: the transport proved the whole text
    /// went to the model in a turn that completed ([`AgentRun::shown_in_full`]).
    SentAgain,
    /// Neither read nor sent.
    NotReviewed,
}

/// The coverage record of one piece.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PieceCoverage {
    pub piece_key: String,
    pub files: BTreeMap<String, FileState>,
    /// Piece files neither read in full nor sent again, in path order.
    pub still_unread: Vec<String>,
    /// Context files the trace shows were read (this covers nothing).
    pub context_read: Vec<String>,
    /// Piece files the room could not serve, with the reason. They are owed, so they are also in
    /// `files` as not reviewed and in `still_unread`.
    pub not_served: BTreeMap<String, String>,
    /// Whether the agent's own `unread.json` named exactly the files the trace shows were not
    /// read in full. `None` when it wrote none.
    pub claim_matched: Option<bool>,
}

/// A path the way the room names it (no leading `./`).
fn plain_path(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

/// Merge inclusive line ranges that overlap or touch.
fn merge(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Compute the coverage of a piece from its runs: the first run and every follow-up on it. The
/// trace is the runs' traces joined; a file counts as sent again only if a run's
/// [`AgentRun::shown_in_full`] lists it (the transport fills that, never a caller). `piece_files`
/// are the files the room serves with their line counts ([`super::Room::piece_line_counts`]);
/// `not_served` are the piece files it could not serve, which stay owed. `context_files` are the
/// context paths. `claimed_unread` is the agent's own list, kept only to measure whether it was
/// true.
pub fn coverage_of(
    piece_key: &str,
    runs: &[AgentRun],
    piece_files: &[(String, usize)],
    not_served: &[NotServed],
    context_files: &[String],
    claimed_unread: Option<&[String]>,
) -> PieceCoverage {
    // A step that did not end `Finished` reviewed nothing (design section 4): neither its reads
    // nor the files it was sent count.
    let done = || runs.iter().filter(|r| r.stop.is_finished());
    let trace: Vec<&ToolEvent> = done().flat_map(|r| r.trace.iter()).collect();
    let sent_again: Vec<&String> = done().flat_map(|r| r.shown_in_full.iter()).collect();
    let mut ranges: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut touched: BTreeMap<String, bool> = BTreeMap::new();
    for event in trace.iter().copied() {
        if let ToolEvent::Read {
            path,
            first_line,
            lines_returned,
            ..
        } = event
        {
            let path = plain_path(path);
            touched.insert(path.clone(), true);
            if *lines_returned > 0 {
                let first = (*first_line).max(1);
                ranges
                    .entry(path)
                    .or_default()
                    .push((first, first + lines_returned - 1));
            }
        }
    }
    let sent: Vec<String> = sent_again.iter().map(|p| plain_path(p)).collect();
    let owed_missing: BTreeMap<String, String> = not_served
        .iter()
        .map(|n| (plain_path(&n.path), n.reason.clone()))
        .collect();
    let mut files = BTreeMap::new();
    let mut still_unread = Vec::new();
    let mut not_full_by_trace: Vec<String> = Vec::new();
    for (path, lines) in piece_files {
        let path = plain_path(path);
        if owed_missing.contains_key(&path) {
            continue;
        }
        let merged = ranges.get(&path).cloned().map(merge).unwrap_or_default();
        let clipped: Vec<(usize, usize)> = if *lines == 0 {
            Vec::new()
        } else {
            merged
                .iter()
                .filter(|(a, _)| a <= lines)
                .map(|&(a, b)| (a, b.min(*lines)))
                .collect()
        };
        let full = if *lines == 0 {
            touched.contains_key(&path)
        } else {
            clipped == vec![(1, *lines)]
        };
        let state = if full {
            FileState::ReadInFull
        } else {
            not_full_by_trace.push(path.clone());
            if sent.contains(&path) {
                FileState::SentAgain
            } else if clipped.is_empty() {
                FileState::NotReviewed
            } else {
                FileState::ReadInPart(clipped)
            }
        };
        if matches!(state, FileState::NotReviewed | FileState::ReadInPart(_)) {
            still_unread.push(path.clone());
        }
        files.insert(path, state);
    }
    // A file the room could not serve was never shown: nothing in the trace or the runs can cover
    // it, so it stays owed whatever a transport reports.
    for path in owed_missing.keys() {
        files.insert(path.clone(), FileState::NotReviewed);
        still_unread.push(path.clone());
    }
    still_unread.sort();
    still_unread.dedup();
    not_full_by_trace.sort();
    let context_read: Vec<String> = context_files
        .iter()
        .map(|p| plain_path(p))
        .filter(|p| ranges.contains_key(p))
        .collect();
    let claim_matched = claimed_unread.map(|claim| {
        let mut claimed: Vec<String> = claim.iter().map(|p| plain_path(p)).collect();
        claimed.sort();
        claimed.dedup();
        claimed == not_full_by_trace
    });
    PieceCoverage {
        piece_key: piece_key.to_string(),
        files,
        still_unread,
        context_read,
        not_served: owed_missing,
        claim_matched,
    }
}
