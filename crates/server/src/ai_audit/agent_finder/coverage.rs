//! Coverage of a piece from its tool trace only (design section 4, MVP checklist item 4: the
//! model's own list never changes a state).

use std::collections::BTreeMap;

use super::ToolEvent;

/// What happened to one file of the piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// The lines returned across all reads cover the whole file.
    ReadInFull,
    /// Some lines were read: the ranges `(first, last)`, 1-based and inclusive, merged.
    ReadInPart(Vec<(usize, usize)>),
    /// Shown to the model in full by the send-again step.
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

/// Compute the coverage of a piece. `piece_files` are the piece's paths with their line counts,
/// `context_files` the context paths, `sent_again` the files the send-again step showed in
/// full, `claimed_unread` the agent's own list (kept only to measure whether it was true).
pub fn coverage_of(
    piece_key: &str,
    trace: &[ToolEvent],
    piece_files: &[(String, usize)],
    context_files: &[String],
    sent_again: &[String],
    claimed_unread: Option<&[String]>,
) -> PieceCoverage {
    let mut ranges: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut touched: BTreeMap<String, bool> = BTreeMap::new();
    for event in trace {
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
    let mut files = BTreeMap::new();
    let mut still_unread = Vec::new();
    let mut not_full_by_trace: Vec<String> = Vec::new();
    for (path, lines) in piece_files {
        let path = plain_path(path);
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
    still_unread.sort();
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
        claim_matched,
    }
}
