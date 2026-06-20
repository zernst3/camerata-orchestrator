//! Provider-agnostic clarify markers: a stable, machine-readable id embedded in
//! a posted clarifying-question comment so a returned answer can be matched back
//! to the exact question round that produced it, and so a retry of the same post
//! is idempotent rather than double-posting.
//!
//! # Why a marker at all
//!
//! The clarify-bridge posts the lead engineer's questions onto the Product
//! Owner's tracker item, the PO answers in a reply, and the bridge polls the
//! answer back. Two problems fall out of that round-trip:
//!
//! 1. **Matching** — the inbound answer is just a `Commented` event on the same
//!    issue. If the issue has unrelated comment chatter (or several open
//!    clarification rounds), the bridge needs a way to know *which* question set
//!    an answer is replying to. A stable id printed into the question comment,
//!    which the PO's reply tends to quote (GitHub/Jira/ADO all quote on reply),
//!    lets the bridge recognise the answer with certainty rather than by
//!    position or timing.
//!
//! 2. **Idempotent posting** — a transient network failure or a retried
//!    reconciliation tick must not post the same questions twice. Because the id
//!    is a deterministic fingerprint of *(work item + questions)*, the bridge can
//!    look for an already-posted comment carrying the same id and skip the second
//!    post.
//!
//! # Marker shape
//!
//! The marker is an HTML comment so it is invisible in rendered Markdown
//! (GitHub, Jira via ADF text, ADO HTML) while remaining present in the raw
//! comment body the API returns on poll:
//!
//! ```text
//! <!-- camerata:clarify:id=clq-3f1a2b9c -->
//! ```
//!
//! The id itself is `clq-` followed by a short hex fingerprint. The fingerprint
//! is a stable, order-sensitive hash of the work item's external id plus the
//! exact question strings, so the same questions on the same item always produce
//! the same id (the property idempotency relies on), while a different question
//! set produces a different id.

use std::hash::{Hash, Hasher};

/// The literal prefix that opens a clarify marker, before the id value.
const MARKER_OPEN: &str = "<!-- camerata:clarify:id=";
/// The literal suffix that closes a clarify marker, after the id value.
const MARKER_CLOSE: &str = " -->";
/// The prefix on every generated question-set id.
const ID_PREFIX: &str = "clq-";

/// Compute the deterministic question-set id for a clarification round.
///
/// The id is a stable function of the work item's `external_id` and the exact
/// ordered list of `questions`. Re-computing it for the same inputs always
/// yields the same id (that is what makes a retried post idempotent); changing
/// any question, their order, or the target item yields a different id.
///
/// The hash is `DefaultHasher` (SipHash). It is NOT cryptographic — it is only a
/// collision-resistant-enough fingerprint for matching answers within a single
/// work item, which is a tiny domain.
pub fn compute_question_set_id(external_id: &str, questions: &[String]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Domain-separate the fields so ("a", ["b"]) and ("ab", []) cannot collide.
    external_id.hash(&mut hasher);
    0xFFu8.hash(&mut hasher); // separator byte between item id and questions
    questions.len().hash(&mut hasher);
    for q in questions {
        q.hash(&mut hasher);
        0x00u8.hash(&mut hasher); // separator byte between questions
    }
    let fingerprint = hasher.finish();
    format!("{ID_PREFIX}{fingerprint:016x}")
}

/// Render the hidden HTML-comment marker carrying `id`, ready to append to a
/// clarifying-question comment body.
pub fn render_marker(id: &str) -> String {
    format!("{MARKER_OPEN}{id}{MARKER_CLOSE}")
}

/// Render the marker for the given work item + questions in one step. Equivalent
/// to `render_marker(&compute_question_set_id(external_id, questions))`.
pub fn marker_for(external_id: &str, questions: &[String]) -> String {
    render_marker(&compute_question_set_id(external_id, questions))
}

/// Extract a clarify question-set id from a comment body, if one is present.
///
/// This tolerates the body being a *quoted reply* (the PO answering above or
/// below a quote of the original question comment), extra whitespace inside the
/// HTML comment, and any surrounding text. It returns the FIRST marker id found,
/// or `None` when the body carries no Camerata clarify marker.
pub fn parse_marker_from_body(body: &str) -> Option<String> {
    let start = body.find(MARKER_OPEN)?;
    let after_open = &body[start + MARKER_OPEN.len()..];
    let end = after_open.find(MARKER_CLOSE)?;
    let raw = after_open[..end].trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

/// Whether `body` carries the specific clarify id `id`. Used to (a) recognise an
/// answer as a reply to a known pending clarification and (b) detect that an
/// equivalent question comment already exists when deciding whether to re-post.
pub fn body_matches_id(body: &str, id: &str) -> bool {
    parse_marker_from_body(body).as_deref() == Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_for_same_inputs() {
        let qs = vec!["Which currency?".to_string(), "Monthly or weekly?".to_string()];
        let a = compute_question_set_id("STORY-1", &qs);
        let b = compute_question_set_id("STORY-1", &qs);
        assert_eq!(a, b, "same item + same questions must yield the same id");
        assert!(a.starts_with("clq-"));
    }

    #[test]
    fn id_changes_when_questions_change() {
        let one = compute_question_set_id("STORY-1", &["Which currency?".to_string()]);
        let two = compute_question_set_id("STORY-1", &["Which timezone?".to_string()]);
        assert_ne!(one, two);
    }

    #[test]
    fn id_changes_when_question_order_changes() {
        let forward = compute_question_set_id(
            "STORY-1",
            &["A?".to_string(), "B?".to_string()],
        );
        let reversed = compute_question_set_id(
            "STORY-1",
            &["B?".to_string(), "A?".to_string()],
        );
        assert_ne!(forward, reversed, "order is significant");
    }

    #[test]
    fn id_changes_when_item_changes() {
        let qs = vec!["Which currency?".to_string()];
        let one = compute_question_set_id("STORY-1", &qs);
        let two = compute_question_set_id("STORY-2", &qs);
        assert_ne!(one, two);
    }

    #[test]
    fn id_is_domain_separated_between_item_and_questions() {
        // ("ab", []) must not collide with ("a", ["b"]) thanks to the separator.
        let a = compute_question_set_id("ab", &[]);
        let b = compute_question_set_id("a", &["b".to_string()]);
        assert_ne!(a, b);
    }

    #[test]
    fn render_then_parse_round_trips() {
        let id = compute_question_set_id("STORY-1", &["Q?".to_string()]);
        let marker = render_marker(&id);
        assert!(marker.starts_with("<!--"));
        assert!(marker.ends_with("-->"));
        assert_eq!(parse_marker_from_body(&marker).as_deref(), Some(id.as_str()));
    }

    #[test]
    fn parse_finds_marker_inside_a_quoted_reply() {
        let id = compute_question_set_id("STORY-1", &["Q?".to_string()]);
        // Simulate a PO reply that quotes the original question comment (which
        // carried the marker) and adds an answer below it.
        let body = format!(
            "> The Camerata orchestrator has the following clarifying questions...\n\
             > - Q?\n\
             > {marker}\n\
             \n\
             USD please.",
            marker = render_marker(&id),
        );
        assert_eq!(parse_marker_from_body(&body).as_deref(), Some(id.as_str()));
    }

    #[test]
    fn parse_tolerates_inner_whitespace() {
        let body = "<!-- camerata:clarify:id=clq-deadbeef -->";
        assert_eq!(parse_marker_from_body(body).as_deref(), Some("clq-deadbeef"));
    }

    #[test]
    fn parse_returns_none_without_a_marker() {
        assert!(parse_marker_from_body("just a normal PO answer, no marker").is_none());
    }

    #[test]
    fn parse_returns_none_on_empty_marker() {
        // A malformed/empty marker id must not be treated as a real id.
        assert!(parse_marker_from_body("<!-- camerata:clarify:id= -->").is_none());
    }

    #[test]
    fn body_matches_id_is_exact() {
        let body = render_marker("clq-1234");
        assert!(body_matches_id(&body, "clq-1234"));
        assert!(!body_matches_id(&body, "clq-9999"));
        assert!(!body_matches_id("no marker here", "clq-1234"));
    }

    #[test]
    fn marker_for_matches_compute_plus_render() {
        let qs = vec!["Q?".to_string()];
        let combined = marker_for("STORY-1", &qs);
        let manual = render_marker(&compute_question_set_id("STORY-1", &qs));
        assert_eq!(combined, manual);
    }
}
