//! A readable diff between two normalised section texts. Sections are one long line after
//! whitespace normalisation, so the diff works on sentence-sized segments (longest common
//! subsequence) and shows only what was removed and what was added.

use serde::{Deserialize, Serialize};

/// Segments compared per side. A section longer than this is compared on its first segments; the
/// hash still covers the whole text.
const MAX_SEGMENTS: usize = 1500;

/// Which side a diff line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffOp {
    /// Present in the previous text, gone now.
    Removed,
    /// New since the previous text.
    Added,
}

/// One changed segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// Removed or added.
    pub op: DiffOp,
    /// The segment's text.
    pub text: String,
}

/// Split `text` after sentence-ending punctuation that is followed by a space.
fn segments(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..bytes.len().saturating_sub(1) {
        if matches!(bytes[i], b'.' | b'!' | b'?') && bytes[i + 1] == b' ' {
            out.push(&text[start..=i]);
            start = i + 2;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out.truncate(MAX_SEGMENTS);
    out
}

/// The removed and added segments turning `old` into `new`, in reading order. Empty when the
/// compared segments are identical (a change past the segment cap, or a pure whitespace change
/// that normalisation already removed, shows no lines).
pub fn diff_sections(old: &str, new: &str) -> Vec<DiffLine> {
    let a = segments(old);
    let b = segments(new);
    // lcs[i][j] = length of the LCS of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut lines = Vec::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j >= b.len() || (i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1]) {
            lines.push(DiffLine {
                op: DiffOp::Removed,
                text: a[i].to_string(),
            });
            i += 1;
        } else {
            lines.push(DiffLine {
                op: DiffOp::Added,
                text: b[j].to_string(),
            });
            j += 1;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_no_diff() {
        assert!(diff_sections("One. Two. Three.", "One. Two. Three.").is_empty());
    }

    #[test]
    fn a_replaced_sentence_shows_one_removal_and_one_addition() {
        let lines = diff_sections(
            "Keep this. Old advice here. Keep that.",
            "Keep this. New advice now. Keep that.",
        );
        assert_eq!(
            lines,
            vec![
                DiffLine {
                    op: DiffOp::Removed,
                    text: "Old advice here.".to_string()
                },
                DiffLine {
                    op: DiffOp::Added,
                    text: "New advice now.".to_string()
                },
            ]
        );
    }

    #[test]
    fn pure_additions_and_removals() {
        let added = diff_sections("A.", "A. B.");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].op, DiffOp::Added);
        let removed = diff_sections("A. B.", "A.");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].op, DiffOp::Removed);
    }
}
