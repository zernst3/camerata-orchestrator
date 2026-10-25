//! The deterministic edges of three rules that are mostly judgment: OWNERSHIP (4), FIELD-VALIDATION
//! (5) and ADMIN-CLAIMS (9).
//!
//! Each states only the part that is a checkable fact (which side of a comparison, whether the
//! incoming document is read at all, whether a role field can be written by its holder). The
//! judgment halves (which collections are per-user, which fields matter, whether claims are the
//! better pattern) belong to the advisory tier, which is why the checker keeps these ids in the
//! advisory prompt.

use std::collections::BTreeSet;

use super::grants::{changes_data, creates, finding, relative_segments, Grant, Site};
use super::rules_model::Target;
use super::rules_parse::{Method, Segment};
use super::{RULE_ADMIN_CLAIMS, RULE_FIELD_VALIDATION, RULE_OWNERSHIP};
use crate::arch_checker::{ArchViolation, ViolationGrade, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::firebase::rules_parse::Service;

fn updates(methods: &BTreeSet<Method>) -> bool {
    super::grants::expand(methods).contains(&Method::Update)
}

fn firestore(site: &Site<'_>) -> bool {
    site.file.service == Service::Firestore
}

/// FIREBASE-RULES-OWNERSHIP-1: an `update` (or `write`) rule that compares the caller only to the
/// INCOMING document. `request.resource` is the document as the caller sends it, so
/// `request.auth.uid == request.resource.data.authorId` lets any signed-in user take over any
/// document by sending their own id; Firebase's own update example checks `resource.data`.
pub(crate) fn ownership(sites: &[Site<'_>]) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    for site in sites.iter().filter(|s| firestore(s)) {
        if !updates(&site.allow.methods) || site.cond.has_disjunction() {
            continue;
        }
        let targets = site.cond.uid_targets(&site.wildcards);
        // Every uid comparison must be against the incoming document; any other target (the
        // stored document, a wildcard, something this reader does not model) is not recognised.
        let incoming: Vec<&str> = targets
            .iter()
            .filter_map(|t| match t {
                Target::Incoming(f) => Some(f.as_str()),
                _ => None,
            })
            .collect();
        if incoming.is_empty() || incoming.len() != targets.len() {
            continue;
        }
        let Some(field) = incoming
            .iter()
            .copied()
            .find(|f| !site.cond.pins_field(f, &site.wildcards))
        else {
            continue;
        };
        let path = site.display_path();
        let body = format!(
            "{} lets a signed-in user update any document under `{path}` by sending `{field}` set to their own id: the rule compares the caller to the INCOMING document (`request.resource.data.{field}`) and never to the stored one (`resource.data.{field}`) (`{}`). Firebase's own update example checks `resource.data`.",
            site.whereabouts(site.allow.line),
            site.allow_text(),
        );
        out.push(finding(
            RULE_OWNERSHIP,
            site.file,
            site.allow.line,
            Some(path),
            SEVERITY_HIGH,
            ViolationGrade::Defect,
            &body,
        ));
    }
    out
}

/// FIREBASE-RULES-FIELD-VALIDATION-1: a write grant that never reads the incoming document, so
/// nothing constrains which fields or types are written. One review-graded row per match block.
pub(crate) fn field_validation(sites: &[Site<'_>]) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<(String, usize)> = BTreeSet::new();
    for site in sites.iter().filter(|s| firestore(s)) {
        let key = (site.file.path.clone(), site.block.line);
        if seen.contains(&key) {
            continue;
        }
        // Any reference to the incoming document, anywhere in the block, makes it silent: partial
        // validation is a judgment, not a fact.
        let block_validates = sites
            .iter()
            .filter(|b| std::ptr::eq(b.block, site.block))
            .any(|b| b.cond.references("request.resource.data"));
        if block_validates {
            seen.insert(key);
            continue;
        }
        let writes_documents = creates(&site.allow.methods) || updates(&site.allow.methods);
        // Open, auth-only and test-mode grants belong to their own rules; `if false` writes
        // nothing; an unrecognised condition is still a write rule that never reads the document.
        let owned_elsewhere = matches!(
            site.grant,
            Grant::Open | Grant::AuthOnly | Grant::TestMode(_) | Grant::Lock
        );
        if !writes_documents || owned_elsewhere {
            continue;
        }
        seen.insert(key);
        let path = site.display_path();
        let body = format!(
            "In `{}`, writes to `{path}` (line {}, `{}`) never look at the incoming document, so nothing is validated: any field of any type can be written, including fields the application treats as privileged such as `role`, `isAdmin`, `credits` or `price`. Firestore documents are schemaless, so the rules are the only place field names and types can be constrained. If writes are validated elsewhere, for example in a Cloud Function, no change is needed.",
            site.file.path,
            site.allow.line,
            site.allow_text(),
        );
        out.push(finding(
            RULE_FIELD_VALIDATION,
            site.file,
            site.allow.line,
            Some(path),
            SEVERITY_MEDIUM,
            ViolationGrade::Review,
            &body,
        ));
    }
    out
}

/// FIREBASE-ADMIN-CLAIMS-1: a role read from a document that its holder may also write. An allow
/// that reads `<collection>/{request.auth.uid}.<field>` grants access by that role; if another
/// rule lets each user write their own `<collection>/{uid}` document without pinning `<field>`,
/// any user can set the role on themselves.
pub(crate) fn admin_claims(sites: &[Site<'_>]) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<(String, usize, String, String)> = BTreeSet::new();
    for grant in sites.iter().filter(|s| firestore(s)) {
        for lookup in grant.cond.role_lookups() {
            let key = (
                grant.file.path.clone(),
                grant.allow.line,
                lookup.collection.clone(),
                lookup.field.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
            // The owner-writable rule for the same collection, one wildcard deep.
            let writer = sites
                .iter()
                .filter(|w| firestore(w) && w.file.path == grant.file.path)
                .filter(|w| owner_can_write_role(w, &lookup.collection, &lookup.field))
                .min_by_key(|w| w.allow.line);
            let Some(writer) = writer else {
                continue;
            };
            let path = format!("/{}/{{uid}}", lookup.collection);
            let body = format!(
                "{} grants access based on the `{field}` field of the caller's own `{coll}` document, and line {wline} (`{wallow}`) lets each user write that document without pinning `{field}`. A user can set `{field}` to an admin value on their own document and gain that access.",
                grant.whereabouts(grant.allow.line),
                field = lookup.field,
                coll = lookup.collection,
                wline = writer.allow.line,
                wallow = writer.allow_text(),
            );
            out.push(finding(
                RULE_ADMIN_CLAIMS,
                grant.file,
                grant.allow.line,
                Some(path),
                SEVERITY_HIGH,
                ViolationGrade::Defect,
                &body,
            ));
        }
    }
    out
}

/// Whether `site` lets the owner of `/<collection>/{w}` create or change their own document
/// without pinning `field`.
fn owner_can_write_role(site: &Site<'_>, collection: &str, field: &str) -> bool {
    let rel = relative_segments(&site.file.service, &site.block.effective_path);
    let [Segment::Literal(c), Segment::Wildcard {
        name,
        recursive: false,
    }] = rel
    else {
        return false;
    };
    if c != collection {
        return false;
    }
    let m = &site.allow.methods;
    if !(creates(m) || updates(m) || changes_data(m)) {
        return false;
    }
    if site.cond.pins_field(field, &site.wildcards) {
        return false;
    }
    let owner_satisfies = match site.grant {
        Grant::Open | Grant::AuthOnly => true,
        Grant::Lock | Grant::TestMode(_) => false,
        Grant::Other => site
            .cond
            .uid_targets(&site.wildcards)
            .iter()
            .any(|t| match t {
                Target::Wildcard(w) => w == name,
                Target::Existing(_) | Target::Incoming(_) => true,
                Target::Other => false,
            }),
    };
    owner_satisfies
}
