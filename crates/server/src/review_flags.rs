//! M4: a wording problem is a recorded flag on the row, never a failure that locks anything.
//!
//! The detail and fix stages used to judge the model's prose with string tests (an added exploit
//! precondition, a name the statement made, an identifier the evidence does not hold, a fix that
//! contradicts its detail, a fix that describes the tool) and wire each verdict to a hard
//! consequence: another paid round, a held run, a locked export. A reviewer reads every row, so
//! a wording problem now costs one re-ask at most; when the text still fails, the best text ships
//! and the failure is written on the row as a flag a reviewer sees.
//!
//! # One list, one capture
//!
//! The flags of a row are one list in the [`REVIEW_FLAGS_CAPTURE`] capture: a JSON array of
//! strings, sorted and without repeats, so the same flags always serialise to the same bytes.
//! The capture is a bookkeeping key (`ai_audit::BOOKKEEPING_CAPTURE_KEYS`), the way M5's
//! `evidence_unverified` is, so two rows carrying the same flag are not read as the same object.
//! The review tree reads this list and turns each entry into an `OutlierReason`.
//!
//! # Names
//!
//! A flag is `name` or `name:argument`. [`PRECONDITION_ADDED`], [`FIX_UNGROUNDED`],
//! [`CONTRADICTION`] and [`METHODOLOGY_LEAK`] stand alone; [`UNVERIFIED_NAME`] carries the token
//! the text dropped (`unverified_name:src/config.rs`). The stage that writes each is fixed by
//! [`stage_of`], which is also how the ledger counts them per stage.

use crate::onboard::Finding;

/// The `captures` key holding a row's review flags (a JSON array of strings).
pub const REVIEW_FLAGS_CAPTURE: &str = "review_flags";

/// Detail stage: the explanation adds an exploit precondition that neither the statement nor the
/// code shown makes.
pub const PRECONDITION_ADDED: &str = "precondition_added";
/// Detail stage: the explanation drops a path or identifier the one-sentence statement named. The
/// flag is `unverified_name:<token>`, one per dropped name.
pub const UNVERIFIED_NAME: &str = "unverified_name";
/// Fix stage: the fix names an identifier found neither in the finding's evidence nor anywhere in
/// the repository.
pub const FIX_UNGROUNDED: &str = "fix_ungrounded";
/// Fix stage: the fix tells the client to do what the finding's own detail says is already done.
pub const CONTRADICTION: &str = "contradiction";
/// Fix stage: the fix describes how the finding was detected instead of what to change.
pub const METHODOLOGY_LEAK: &str = "methodology_leak";

/// The pipeline stage that writes a flag. The ledger counts per stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlagStage {
    /// The detail (client explanation) stage.
    Detail,
    /// The fix stage.
    Fix,
    /// The content check at seal (`junk_finding`): wording judgments of a row's text.
    Content,
}

/// The name of a flag, without its argument.
pub fn kind_of(flag: &str) -> &str {
    flag.split_once(':').map_or(flag, |(kind, _)| kind)
}

/// The stage that writes `flag`. A flag no stage writes is a content flag.
pub fn stage_of(flag: &str) -> FlagStage {
    match kind_of(flag) {
        PRECONDITION_ADDED | UNVERIFIED_NAME => FlagStage::Detail,
        FIX_UNGROUNDED | CONTRADICTION | METHODOLOGY_LEAK => FlagStage::Fix,
        _ => FlagStage::Content,
    }
}

/// The flags on `f`, sorted. An absent or unreadable capture is no flags.
pub fn flags_of(f: &Finding) -> Vec<String> {
    f.captures
        .get(REVIEW_FLAGS_CAPTURE)
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
}

fn store(f: &mut Finding, mut flags: Vec<String>) {
    flags.sort();
    flags.dedup();
    if flags.is_empty() {
        f.captures.remove(REVIEW_FLAGS_CAPTURE);
    } else {
        // A list of strings always serialises.
        let raw = serde_json::to_string(&flags).unwrap_or_default();
        f.captures.insert(REVIEW_FLAGS_CAPTURE.to_string(), raw);
    }
}

/// Replace the flags `stage` wrote on `f` with `new`, leaving every other stage's flags alone, and
/// return the flags now recorded for that stage (sorted, without repeats). Empty `new` clears the
/// stage's flags: a row whose text now passes carries none from an earlier attempt.
pub fn set_stage_flags(f: &mut Finding, stage: FlagStage, new: Vec<String>) -> Vec<String> {
    let mut flags: Vec<String> = flags_of(f)
        .into_iter()
        .filter(|flag| stage_of(flag) != stage)
        .collect();
    let mut mine = new;
    mine.sort();
    mine.dedup();
    flags.extend(mine.iter().cloned());
    store(f, flags);
    mine
}

/// Every flag on `findings`, counted per stage. The reconciliation side of the ledger's counts: a
/// stage that recorded N flags left N on the rows (until a later step drops or merges a row).
pub fn count_on_rows(findings: &[Finding]) -> std::collections::BTreeMap<FlagStage, u64> {
    let mut out = std::collections::BTreeMap::new();
    for f in findings {
        for flag in flags_of(f) {
            *out.entry(stage_of(&flag)).or_insert(0) += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_one_sorted_list_without_repeats() {
        let mut f = Finding::default();
        set_stage_flags(
            &mut f,
            FlagStage::Detail,
            vec![
                "unverified_name:b.rs".to_string(),
                PRECONDITION_ADDED.to_string(),
                PRECONDITION_ADDED.to_string(),
            ],
        );
        assert_eq!(
            f.captures.get(REVIEW_FLAGS_CAPTURE).map(String::as_str),
            Some(r#"["precondition_added","unverified_name:b.rs"]"#)
        );
        assert_eq!(flags_of(&f).len(), 2);
    }

    #[test]
    fn a_stage_replaces_only_its_own_flags() {
        let mut f = Finding::default();
        set_stage_flags(&mut f, FlagStage::Detail, vec![PRECONDITION_ADDED.into()]);
        set_stage_flags(&mut f, FlagStage::Fix, vec![FIX_UNGROUNDED.into()]);
        set_stage_flags(&mut f, FlagStage::Detail, Vec::new());
        assert_eq!(flags_of(&f), vec![FIX_UNGROUNDED.to_string()]);
        set_stage_flags(&mut f, FlagStage::Fix, Vec::new());
        assert!(!f.captures.contains_key(REVIEW_FLAGS_CAPTURE));
    }

    #[test]
    fn each_flag_belongs_to_one_stage() {
        assert_eq!(stage_of("unverified_name:src/a.rs"), FlagStage::Detail);
        assert_eq!(stage_of(PRECONDITION_ADDED), FlagStage::Detail);
        assert_eq!(stage_of(FIX_UNGROUNDED), FlagStage::Fix);
        assert_eq!(stage_of(CONTRADICTION), FlagStage::Fix);
        assert_eq!(stage_of(METHODOLOGY_LEAK), FlagStage::Fix);
        assert_eq!(stage_of("placeholder_wording"), FlagStage::Content);
    }

    #[test]
    fn an_unreadable_capture_is_no_flags() {
        let mut f = Finding::default();
        f.captures
            .insert(REVIEW_FLAGS_CAPTURE.to_string(), "not json".to_string());
        assert!(flags_of(&f).is_empty());
    }
}
