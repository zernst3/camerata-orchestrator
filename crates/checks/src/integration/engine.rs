//! The generic, stack-agnostic reconciliation engine.
//!
//! This is the cross-agent sibling of the per-agent [`crate::RustCheckRunner`] /
//! [`camerata_core::CheckRunner`]: where a `CheckRunner` evaluates ONE agent's
//! worktree, the engine here evaluates the ASSEMBLED tree — the combined
//! produced/consumed lists of every in-scope repo — and emits a BINARY,
//! reproducible verdict per selected `INTEGRATION-*` rule.
//!
//! # No stack strings in this layer (the hard invariant)
//!
//! Every function here operates on the neutral [`crate::integration::vocab`]
//! types. There is not one `match language` in this module. A shared compiled
//! type across a Rust boundary is not special-cased: it is simply the input where
//! the extractor emitted matching produced/consumed records, so the engine finds
//! zero drift. The reconciliation LOGIC is 100% generic; only the
//! [`crate::integration::extractor`] layer is stack-aware.
//!
//! # Determinism (the ADR's hard line)
//!
//! The verdict is a deterministic comparison of neutral records: identity match
//! on method+path (or kind+name), then optional shape comparison when BOTH sides
//! carry a shape. No model is ever consulted. Where a seam CANNOT be made
//! deterministic on a given stack (no extractor), that seam is surfaced
//! REVIEW-TIER by the caller — never rendered green. See
//! [`crate::integration::rules`] for the review-tier fallback.

use std::collections::BTreeMap;

use crate::integration::vocab::{
    normalize_field, ArtifactKind, Consumed, Produced, RepoArtifacts, Shape,
};

/// The severity of a single reconciliation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingLevelKind {
    /// A hard, deterministic mismatch: the gate FAILS (bounce/escalate).
    Fail,
    /// A seam that could not be checked deterministically on this stack: routed
    /// to human QA. NEVER rendered as a pass; NEVER a fail. Honestly labeled.
    Review,
}

/// One reconciliation finding produced by the engine for a selected rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The `INTEGRATION-*` rule id this finding belongs to.
    pub rule_id: String,
    /// Fail (deterministic mismatch) or Review (undeterminable seam).
    pub level: FindingLevelKind,
    /// The `owner/repo` most responsible (the consumer, or the producer for a
    /// dangling emit). Used by the verdict router to bounce the right agent.
    pub repo: String,
    /// A stable identity string for the offending artifact (for waiver matching
    /// and dedup). Mirrors [`ArtifactKind::identity`].
    pub artifact: String,
    /// Human-readable delta: exactly what did not line up.
    pub detail: String,
    /// Best-effort `path:line` of the offending consumer/producer.
    pub location: String,
}

/// The assembled, cross-agent view: every repo's normalized artifacts, indexed
/// for reconciliation. Built once per gate run from the per-repo extractor
/// outputs.
#[derive(Debug, Clone, Default)]
pub struct AssembledTree {
    /// All produced artifacts across every repo.
    pub produced: Vec<Produced>,
    /// All consumed usages across every repo.
    pub consumed: Vec<Consumed>,
}

impl AssembledTree {
    /// Assemble the tree from each repo's extracted artifacts. Order-independent
    /// and deterministic: producers/consumers are concatenated then the lists are
    /// sorted by identity so the engine's iteration order (and thus the finding
    /// order) is stable across runs and machines.
    pub fn from_repos(repos: &[RepoArtifacts]) -> Self {
        let mut produced: Vec<Produced> = Vec::new();
        let mut consumed: Vec<Consumed> = Vec::new();
        for r in repos {
            produced.extend(r.produced.iter().cloned());
            consumed.extend(r.consumed.iter().cloned());
        }
        produced.sort_by(|a, b| {
            a.kind
                .identity()
                .cmp(&b.kind.identity())
                .then_with(|| a.repo.cmp(&b.repo))
        });
        consumed.sort_by(|a, b| {
            a.kind
                .identity()
                .cmp(&b.kind.identity())
                .then_with(|| a.repo.cmp(&b.repo))
        });
        AssembledTree { produced, consumed }
    }

    /// Index producers by artifact identity for O(1) lookup during reconciliation.
    fn produced_index(&self) -> BTreeMap<String, Vec<&Produced>> {
        let mut idx: BTreeMap<String, Vec<&Produced>> = BTreeMap::new();
        for p in &self.produced {
            idx.entry(p.kind.identity()).or_default().push(p);
        }
        idx
    }
}

/// The rule this pass is evaluating. The engine's reconciliation primitives are
/// generic; a `SeamRule` selects WHICH relation to enforce over the assembled
/// tree. Distinct from the corpus [`camerata_rules::Rule`] (that is the human
/// selection + prose); this is the engine's executable discriminant, resolved
/// from the corpus rule id in [`crate::integration::rules`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeamRule {
    /// INTEGRATION-API-CONTRACT-1: every consumed endpoint matches a produced
    /// route (identity), and where both carry a shape, the shapes agree.
    ApiContract,
    /// INTEGRATION-EVENT-WIRING-1: every emitted event has a consumer and every
    /// subscribed event is emitted (no dangling ends, both directions).
    EventWiring,
    /// INTEGRATION-AUTH-SEAM-1: every UI-gated affordance maps to a produced
    /// endpoint that ENFORCES a guard. Fires PER SEAM: only for consumptions the
    /// UI actually gates; a public endpoint the UI does not gate is out of scope.
    AuthSeam,
}

impl SeamRule {
    /// The corpus rule id this executable rule corresponds to.
    pub fn rule_id(&self) -> &'static str {
        match self {
            SeamRule::ApiContract => "INTEGRATION-API-CONTRACT-1",
            SeamRule::EventWiring => "INTEGRATION-EVENT-WIRING-1",
            SeamRule::AuthSeam => "INTEGRATION-AUTH-SEAM-1",
        }
    }

    /// Map a corpus rule id back to an executable [`SeamRule`]. `None` when the id
    /// is not one the engine knows how to execute (a rule with no engine seam is
    /// review-tier or out of scope).
    pub fn from_rule_id(id: &str) -> Option<Self> {
        match id {
            "INTEGRATION-API-CONTRACT-1" => Some(SeamRule::ApiContract),
            "INTEGRATION-EVENT-WIRING-1" => Some(SeamRule::EventWiring),
            "INTEGRATION-AUTH-SEAM-1" => Some(SeamRule::AuthSeam),
            _ => None,
        }
    }
}

/// Reconcile the assembled tree against ONE seam rule and return every finding.
///
/// Deterministic and pure: same input → same ordered output, no I/O, no model.
/// An empty return means the seam holds (a PASS for this rule). Findings carry
/// their own [`FindingLevelKind`]; the caller separates Fail (bounce) from
/// Review (human QA) and applies waivers.
pub fn reconcile(tree: &AssembledTree, rule: SeamRule) -> Vec<Finding> {
    match rule {
        SeamRule::ApiContract => reconcile_api_contract(tree),
        SeamRule::EventWiring => reconcile_event_wiring(tree),
        SeamRule::AuthSeam => reconcile_auth_seam(tree),
    }
}

/// INTEGRATION-API-CONTRACT-1: consumer endpoint calls must match a producer route.
fn reconcile_api_contract(tree: &AssembledTree) -> Vec<Finding> {
    let idx = tree.produced_index();
    let mut findings = Vec::new();
    for c in &tree.consumed {
        // Only endpoints participate in this rule.
        if !matches!(c.kind, ArtifactKind::Endpoint { .. }) {
            continue;
        }
        let id = c.kind.identity();
        match idx.get(&id) {
            None => {
                findings.push(Finding {
                    rule_id: SeamRule::ApiContract.rule_id().to_string(),
                    level: FindingLevelKind::Fail,
                    repo: c.repo.clone(),
                    artifact: id.clone(),
                    detail: format!(
                        "{} calls {} but no producer exposes that route. \
                         Either the consumer targets the wrong method/path (a casing or \
                         path-shape drift), or the producer never shipped it.",
                        c.repo,
                        endpoint_human(&c.kind),
                    ),
                    location: c.location.clone(),
                });
            }
            Some(producers) => {
                // A route exists. If BOTH sides carry a non-empty shape, compare.
                if let Some(cshape) = c.shape.as_ref().filter(|s| !s.is_empty()) {
                    // Compare against every producer of this identity; a match on
                    // any producer clears the consumer.
                    let matched = producers.iter().any(|p| {
                        p.shape
                            .as_ref()
                            .filter(|s| !s.is_empty())
                            .is_none_or(|pshape| shapes_agree(pshape, cshape))
                    });
                    if !matched {
                        // Every producer that carried a shape disagreed.
                        let deltas: Vec<String> = producers
                            .iter()
                            .filter_map(|p| p.shape.as_ref().filter(|s| !s.is_empty()))
                            .map(|pshape| shape_delta(pshape, cshape))
                            .collect();
                        findings.push(Finding {
                            rule_id: SeamRule::ApiContract.rule_id().to_string(),
                            level: FindingLevelKind::Fail,
                            repo: c.repo.clone(),
                            artifact: id.clone(),
                            detail: format!(
                                "{} calls {} but the request/response shape drifts from the \
                                 producer: {}.",
                                c.repo,
                                endpoint_human(&c.kind),
                                deltas.join("; "),
                            ),
                            location: c.location.clone(),
                        });
                    }
                }
                // If the consumer carries no shape (or the producer carries none),
                // identity match alone is a PASS: absence of shape evidence is
                // never a drift finding (the ADR's honesty stance).
            }
        }
    }
    findings
}

/// INTEGRATION-EVENT-WIRING-1: no dangling emit and no dangling subscribe.
fn reconcile_event_wiring(tree: &AssembledTree) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Emitted-but-unconsumed: a produced Event with no matching consumed Event.
    for p in &tree.produced {
        if !matches!(p.kind, ArtifactKind::Event { .. }) {
            continue;
        }
        let id = p.kind.identity();
        let has_consumer = tree
            .consumed
            .iter()
            .any(|c| matches!(c.kind, ArtifactKind::Event { .. }) && c.kind.identity() == id);
        if !has_consumer {
            findings.push(Finding {
                rule_id: SeamRule::EventWiring.rule_id().to_string(),
                level: FindingLevelKind::Fail,
                repo: p.repo.clone(),
                artifact: id.clone(),
                detail: format!(
                    "{} emits {} but no agent subscribes to it — a dangling emit. \
                     Either a consumer is missing, or the event should not be emitted.",
                    p.repo,
                    event_human(&p.kind),
                ),
                location: p.location.clone(),
            });
        }
    }

    // Subscribed-but-unemitted: a consumed Event with no matching produced Event.
    for c in &tree.consumed {
        if !matches!(c.kind, ArtifactKind::Event { .. }) {
            continue;
        }
        let id = c.kind.identity();
        let has_emitter = tree
            .produced
            .iter()
            .any(|p| matches!(p.kind, ArtifactKind::Event { .. }) && p.kind.identity() == id);
        if !has_emitter {
            findings.push(Finding {
                rule_id: SeamRule::EventWiring.rule_id().to_string(),
                level: FindingLevelKind::Fail,
                repo: c.repo.clone(),
                artifact: id.clone(),
                detail: format!(
                    "{} subscribes to {} but no agent emits it — a dangling subscription. \
                     The producer never publishes the event the consumer waits on.",
                    c.repo,
                    event_human(&c.kind),
                ),
                location: c.location.clone(),
            });
        }
    }

    findings
}

/// INTEGRATION-AUTH-SEAM-1: every UI-gated affordance maps to a guarded endpoint.
///
/// PER-SEAM firing (the false-positive guard): this rule fires ONLY for
/// consumptions where `ui_gated == Some(true)`. A public endpoint the UI does not
/// gate is entirely out of scope — no finding. Where the producer's guard status
/// is UNKNOWN (`guarded == None`), the seam is REVIEW-TIER (undeterminable), never
/// a silent pass. Where the producer is known-unguarded, it is a FAIL.
fn reconcile_auth_seam(tree: &AssembledTree) -> Vec<Finding> {
    let idx = tree.produced_index();
    let mut findings = Vec::new();
    for c in &tree.consumed {
        if !matches!(c.kind, ArtifactKind::Endpoint { .. }) {
            continue;
        }
        // Per-seam: only affordances the UI actually gates are in scope.
        if c.ui_gated != Some(true) {
            continue;
        }
        let id = c.kind.identity();
        match idx.get(&id) {
            None => {
                // A gated affordance calling a route nobody exposes is also a
                // contract break; report it here so the auth seam alone catches it.
                findings.push(Finding {
                    rule_id: SeamRule::AuthSeam.rule_id().to_string(),
                    level: FindingLevelKind::Fail,
                    repo: c.repo.clone(),
                    artifact: id.clone(),
                    detail: format!(
                        "{} gates a UI affordance on {} but no producer exposes that endpoint, \
                         so the permission cannot be enforced server-side.",
                        c.repo,
                        endpoint_human(&c.kind),
                    ),
                    location: c.location.clone(),
                });
            }
            Some(producers) => {
                // Guard status across the producers of this endpoint.
                let any_guarded = producers.iter().any(|p| p.guarded == Some(true));
                let all_known = producers.iter().all(|p| p.guarded.is_some());
                if any_guarded {
                    // A guarded producer exists → seam holds. PASS.
                } else if all_known {
                    // Every producer is known and NONE guards → deterministic FAIL.
                    let loc = producers
                        .iter()
                        .find(|p| !p.location.is_empty())
                        .map(|p| p.location.clone())
                        .unwrap_or_else(|| c.location.clone());
                    findings.push(Finding {
                        rule_id: SeamRule::AuthSeam.rule_id().to_string(),
                        level: FindingLevelKind::Fail,
                        repo: producers[0].repo.clone(),
                        artifact: id.clone(),
                        detail: format!(
                            "{} gates a UI affordance on {}, but the producer endpoint enforces \
                             NO server-side guard — the UI hides the button while the endpoint is \
                             open. Add the permission check to the endpoint, or waive with \
                             `camerata:allow {} -- <reason>` if the endpoint is intentionally public.",
                            c.repo,
                            endpoint_human(&c.kind),
                            SeamRule::AuthSeam.rule_id(),
                        ),
                        location: loc,
                    });
                } else {
                    // Guard status undeterminable → REVIEW-TIER (honest, not green).
                    findings.push(Finding {
                        rule_id: SeamRule::AuthSeam.rule_id().to_string(),
                        level: FindingLevelKind::Review,
                        repo: producers[0].repo.clone(),
                        artifact: id.clone(),
                        detail: format!(
                            "{} gates a UI affordance on {}; the extractor could not determine \
                             whether the producer endpoint enforces a guard. Routed to human QA \
                             (review-tier) — NOT passed.",
                            c.repo,
                            endpoint_human(&c.kind),
                        ),
                        location: c.location.clone(),
                    });
                }
            }
        }
    }
    findings
}

/// True when a producer shape and a consumer shape agree closely enough that the
/// seam holds: the consumer must not read a RESPONSE field the producer does not
/// emit, and must not send a REQUEST field the producer does not accept.
///
/// Field presence is matched on [`normalize_field`], so `member_id` (producer)
/// and `memberId` (consumer) are the same field. Status codes agree when every
/// status the consumer HANDLES is one the producer can RETURN (a consumer that
/// handles a superset is fine; one expecting a code the producer never returns is
/// a drift — but only checked when both sides declare codes).
fn shapes_agree(producer: &Shape, consumer: &Shape) -> bool {
    field_subset(&consumer.response_fields, &producer.response_fields)
        && field_subset(&consumer.request_fields, &producer.request_fields)
        && status_subset(consumer, producer)
}

/// True when every field in `needed` is present (by normalized name) in `have`.
/// An empty `needed` is trivially satisfied (nothing to check on that axis).
fn field_subset(needed: &[String], have: &[String]) -> bool {
    let have_norm: Vec<String> = have.iter().map(|f| normalize_field(f)).collect();
    needed
        .iter()
        .all(|f| have_norm.contains(&normalize_field(f)))
}

/// True when the consumer's declared status codes are a subset of the producer's.
/// Only checked when BOTH declare codes; otherwise trivially satisfied.
fn status_subset(consumer: &Shape, producer: &Shape) -> bool {
    if consumer.status_codes.is_empty() || producer.status_codes.is_empty() {
        return true;
    }
    consumer
        .status_codes
        .iter()
        .all(|c| producer.status_codes.contains(c))
}

/// A human description of exactly how two shapes differ (for the finding detail).
fn shape_delta(producer: &Shape, consumer: &Shape) -> String {
    let mut parts = Vec::new();
    let missing_resp: Vec<&String> = missing(&consumer.response_fields, &producer.response_fields);
    if !missing_resp.is_empty() {
        parts.push(format!(
            "consumer reads response field(s) the producer does not emit: {}",
            join_refs(&missing_resp)
        ));
    }
    let missing_req: Vec<&String> = missing(&consumer.request_fields, &producer.request_fields);
    if !missing_req.is_empty() {
        parts.push(format!(
            "consumer sends request field(s) the producer does not accept: {}",
            join_refs(&missing_req)
        ));
    }
    if !status_subset(consumer, producer) {
        let extra: Vec<&u16> = consumer
            .status_codes
            .iter()
            .filter(|c| !producer.status_codes.contains(c))
            .collect();
        parts.push(format!(
            "consumer handles status code(s) the producer never returns: {}",
            extra
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if parts.is_empty() {
        "shape mismatch".to_string()
    } else {
        parts.join("; ")
    }
}

/// Fields in `needed` absent (by normalized name) from `have`.
fn missing<'a>(needed: &'a [String], have: &[String]) -> Vec<&'a String> {
    let have_norm: Vec<String> = have.iter().map(|f| normalize_field(f)).collect();
    needed
        .iter()
        .filter(|f| !have_norm.contains(&normalize_field(f)))
        .collect()
}

fn join_refs(v: &[&String]) -> String {
    v.iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn endpoint_human(kind: &ArtifactKind) -> String {
    match kind {
        ArtifactKind::Endpoint { method, path } => format!("`{} {}`", method.to_uppercase(), path),
        other => other.identity(),
    }
}

fn event_human(kind: &ArtifactKind) -> String {
    match kind {
        ArtifactKind::Event { name } => format!("event `{name}`"),
        other => other.identity(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::vocab::{ArtifactKind, Consumed, Produced, RepoArtifacts, Shape};

    fn ep(method: &str, path: &str) -> ArtifactKind {
        ArtifactKind::Endpoint {
            method: method.to_string(),
            path: crate::integration::vocab::normalize_path(path),
        }
    }

    fn produced_ep(repo: &str, method: &str, path: &str) -> Produced {
        Produced {
            repo: repo.into(),
            kind: ep(method, path),
            shape: None,
            guarded: None,
            location: format!("{repo}/routes.rs:1"),
        }
    }

    fn consumed_ep(repo: &str, method: &str, path: &str) -> Consumed {
        Consumed {
            repo: repo.into(),
            kind: ep(method, path),
            shape: None,
            ui_gated: None,
            location: format!("{repo}/client.ts:1"),
        }
    }

    fn tree(produced: Vec<Produced>, consumed: Vec<Consumed>) -> AssembledTree {
        AssembledTree::from_repos(&[RepoArtifacts {
            repo: "mixed".into(),
            produced,
            consumed,
        }])
    }

    #[test]
    fn matching_endpoint_passes() {
        let t = tree(
            vec![produced_ep("api", "GET", "/users/:id")],
            vec![consumed_ep("ui", "GET", "/users/{id}")],
        );
        assert!(reconcile(&t, SeamRule::ApiContract).is_empty());
    }

    #[test]
    fn consumer_calls_missing_route_fails() {
        let t = tree(
            vec![produced_ep("api", "POST", "/members/export")],
            vec![consumed_ep("ui", "POST", "/members/csv")],
        );
        let f = reconcile(&t, SeamRule::ApiContract);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, FindingLevelKind::Fail);
        assert_eq!(f[0].repo, "ui");
    }

    #[test]
    fn casing_drift_in_path_fails() {
        let t = tree(
            vec![produced_ep("api", "GET", "/Members")],
            vec![consumed_ep("ui", "GET", "/members")],
        );
        let f = reconcile(&t, SeamRule::ApiContract);
        assert_eq!(f.len(), 1, "literal-casing drift is a real mismatch");
    }

    #[test]
    fn shape_drift_fails_field_read_not_emitted() {
        let mut p = produced_ep("api", "GET", "/members/:id");
        p.shape = Some(Shape {
            response_fields: vec!["member_id".into(), "name".into()],
            ..Default::default()
        });
        let mut c = consumed_ep("ui", "GET", "/members/{id}");
        c.shape = Some(Shape {
            response_fields: vec!["memberId".into(), "email".into()],
            ..Default::default()
        });
        let f = reconcile(&tree(vec![p], vec![c]), SeamRule::ApiContract);
        assert_eq!(f.len(), 1);
        assert!(f[0].detail.contains("email"), "email is not emitted: {}", f[0].detail);
    }

    #[test]
    fn casing_only_field_difference_is_not_a_drift() {
        // member_id (producer) vs memberId (consumer) — same field, no finding.
        let mut p = produced_ep("api", "GET", "/members/:id");
        p.shape = Some(Shape {
            response_fields: vec!["member_id".into()],
            ..Default::default()
        });
        let mut c = consumed_ep("ui", "GET", "/members/{id}");
        c.shape = Some(Shape {
            response_fields: vec!["memberId".into()],
            ..Default::default()
        });
        assert!(reconcile(&tree(vec![p], vec![c]), SeamRule::ApiContract).is_empty());
    }

    #[test]
    fn emitted_but_unconsumed_event_fails() {
        let t = tree(
            vec![Produced {
                repo: "api".into(),
                kind: ArtifactKind::Event { name: "member.created".into() },
                shape: None,
                guarded: None,
                location: "api/events.rs:5".into(),
            }],
            vec![],
        );
        let f = reconcile(&t, SeamRule::EventWiring);
        assert_eq!(f.len(), 1);
        assert!(f[0].detail.contains("dangling emit"));
    }

    #[test]
    fn subscribed_but_unemitted_event_fails() {
        let t = tree(
            vec![],
            vec![Consumed {
                repo: "worker".into(),
                kind: ArtifactKind::Event { name: "member.created".into() },
                shape: None,
                ui_gated: None,
                location: "worker/sub.ts:3".into(),
            }],
        );
        let f = reconcile(&t, SeamRule::EventWiring);
        assert_eq!(f.len(), 1);
        assert!(f[0].detail.contains("dangling subscription"));
    }

    #[test]
    fn fully_wired_event_passes() {
        let t = tree(
            vec![Produced {
                repo: "api".into(),
                kind: ArtifactKind::Event { name: "e".into() },
                shape: None,
                guarded: None,
                location: String::new(),
            }],
            vec![Consumed {
                repo: "worker".into(),
                kind: ArtifactKind::Event { name: "e".into() },
                shape: None,
                ui_gated: None,
                location: String::new(),
            }],
        );
        assert!(reconcile(&t, SeamRule::EventWiring).is_empty());
    }

    #[test]
    fn auth_seam_gated_affordance_without_guard_fails() {
        let mut p = produced_ep("api", "POST", "/members/:id/ban");
        p.guarded = Some(false);
        let mut c = consumed_ep("ui", "POST", "/members/{id}/ban");
        c.ui_gated = Some(true);
        let f = reconcile(&tree(vec![p], vec![c]), SeamRule::AuthSeam);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, FindingLevelKind::Fail);
    }

    #[test]
    fn auth_seam_ungated_public_endpoint_is_not_flagged() {
        // The UI does NOT gate this call (ui_gated None), so the rule is out of
        // scope even though the endpoint is unguarded. No false positive.
        let mut p = produced_ep("api", "GET", "/health");
        p.guarded = Some(false);
        let c = consumed_ep("ui", "GET", "/health"); // ui_gated None
        assert!(reconcile(&tree(vec![p], vec![c]), SeamRule::AuthSeam).is_empty());
    }

    #[test]
    fn auth_seam_gated_with_guard_passes() {
        let mut p = produced_ep("api", "POST", "/members/:id/ban");
        p.guarded = Some(true);
        let mut c = consumed_ep("ui", "POST", "/members/{id}/ban");
        c.ui_gated = Some(true);
        assert!(reconcile(&tree(vec![p], vec![c]), SeamRule::AuthSeam).is_empty());
    }

    #[test]
    fn auth_seam_unknown_guard_is_review_tier_not_pass() {
        let mut p = produced_ep("api", "POST", "/x");
        p.guarded = None; // undeterminable
        let mut c = consumed_ep("ui", "POST", "/x");
        c.ui_gated = Some(true);
        let f = reconcile(&tree(vec![p], vec![c]), SeamRule::AuthSeam);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, FindingLevelKind::Review);
    }

    #[test]
    fn assembly_is_order_independent() {
        let a = AssembledTree::from_repos(&[
            RepoArtifacts { repo: "a".into(), produced: vec![produced_ep("a", "GET", "/z")], consumed: vec![] },
            RepoArtifacts { repo: "b".into(), produced: vec![produced_ep("b", "GET", "/a")], consumed: vec![] },
        ]);
        let b = AssembledTree::from_repos(&[
            RepoArtifacts { repo: "b".into(), produced: vec![produced_ep("b", "GET", "/a")], consumed: vec![] },
            RepoArtifacts { repo: "a".into(), produced: vec![produced_ep("a", "GET", "/z")], consumed: vec![] },
        ]);
        assert_eq!(a.produced, b.produced, "assembly order is stable");
    }
}
