//! W25 — no directive references CAMERATA'S OWN enforcement plumbing.
//!
//! A directive ships verbatim into the client's `AGENTS.md`/`CONVENTIONS.md` as a convention
//! their project follows. A client has no "VCS gate", no "layer 4 (CI) check", no "assembled
//! tree", and runs no "integration gate reconciliation engine" — those are names for
//! Camerata's own topology and UI. Emitting them tells the client to follow a convention
//! phrased in the vendor's internal vocabulary, which they cannot act on.
//!
//! This is distinct from `w25_directive_voice` (which catches scanner-VERB openers like
//! "Scan…"/"Flag as…"): these directives open in perfectly good convention voice ("Every
//! commit subject contains…") and then reference Camerata machinery mid-sentence. The
//! convention each expresses is valid and stays; only the vendor plumbing is stripped, with
//! any genuine detection mechanics moved to `qualifies` (author-facing, never emitted).

use camerata_rules::{corpus_path, load_corpus_lenient};

/// Phrases that name Camerata's own enforcement topology or configuration UI. Each is specific
/// enough not to collide with a legitimate client convention — a client directive would say
/// "your CI", not "layer 4 (CI)"; "across the whole system", not "the assembled tree".
// NOTE: bare "Camerata" is deliberately NOT banned. A rule whose SUBJECT is adopting a
// Camerata-provisioned check (the CICD-DEPENDENCY-AUDIT / CICD-SEMGREP tool-wiring rules)
// legitimately names the tool the client is adopting — that is a separate open question (the
// G1 tool-adoption carve-out) for the owner, not internal-topology plumbing. This gate bans
// only the topology/UI terms, which no client convention would ever contain.
const VENDOR_PLUMBING: &[&str] = &[
    "VCS gate",
    "VCS-action gate",
    "layer 4 (CI)",
    "layer-2 code gate",
    "layer-2 gate",
    "per-agent Layer-1",
    "project Settings",
    "Settings →",
    "integration gate",
    "reconciliation engine",
    "the assembled tree",
    "assembled tree",
    "role agents",
];

#[tokio::test]
async fn req_w25_no_directive_names_camerata_plumbing() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let mut offenders: Vec<String> = Vec::new();
    for rule in corpus.iter() {
        for opt in &rule.options {
            let hits: Vec<&str> = VENDOR_PLUMBING
                .iter()
                .copied()
                .filter(|p| opt.directive.contains(p))
                .collect();
            if !hits.is_empty() {
                offenders.push(format!("{} / {}: {:?}", rule.id.0, opt.id, hits));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these directives ship Camerata's own enforcement vocabulary into the client's \
         CONVENTIONS.md; state the convention in the client's terms and move detection \
         mechanics to `qualifies`:\n  {}",
        offenders.join("\n  ")
    );
}
