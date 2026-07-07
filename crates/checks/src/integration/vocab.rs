//! The neutral cross-agent vocabulary: the small, stack-agnostic type set the
//! reconciliation engine reasons over.
//!
//! # Why a neutral vocabulary (the stack-generalization invariant)
//!
//! The integration gate is STACK-GENERALIZED. Nothing about a particular stack
//! (Rust vs JavaScript vs Go) is baked into the engine. The only stack-aware code
//! is the per-stack [`crate::integration::extractor::Extractor`] that turns a
//! repo's source into two lists over THIS vocabulary:
//!
//! - [`Produced`] artifacts — what an agent's repo EXPOSES (a route it serves, a
//!   type it defines, an event it emits, an entity it migrates, a config key it
//!   declares).
//! - [`Consumed`] usages — what an agent's repo DEPENDS ON from another (a route
//!   it calls, a type it imports across the seam, an event it subscribes to, an
//!   entity it references, a config key it reads).
//!
//! Once both sides are normalized into this vocabulary, "does the consumer's call
//! match a producer's route?" is a deterministic comparison of neutral records —
//! never an LLM eyeballing prose. A shared compiled type (the Rust monorepo case)
//! is simply the case where the extractor finds ZERO drift; it is not a different
//! mechanism. See `docs/decisions/2026-07-05_integration-gate-generic-engine.md`.

use serde::{Deserialize, Serialize};

/// An HTTP method, normalized to upper-case. Stored as a small string so the
/// vocabulary carries no stack-specific enum surface (a stack that invents a
/// method the engine has never seen still round-trips as a string).
pub type Method = String;

/// The KIND of a cross-agent artifact. Deliberately small: the seams that
/// generalize cleanly across stacks. Richer kinds (full schema, migration DDL)
/// are staged; see the ADR.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// An HTTP endpoint / route. The workhorse seam: routes normalize cleanly
    /// across every web stack (method + path + optional shape + status codes).
    Endpoint {
        /// Upper-cased HTTP method (`GET`, `POST`, ...).
        method: Method,
        /// The route path, NORMALIZED (see [`normalize_path`]): leading slash,
        /// no trailing slash, path params collapsed to `{}`.
        path: String,
    },
    /// A shared DTO / type / enum crossing the boundary, identified by name.
    Type {
        /// The type name, as the stack spells it (extractor may normalize casing).
        name: String,
    },
    /// A domain event / message, identified by name.
    Event {
        /// The event name (e.g. `member.created`).
        name: String,
    },
    /// A persisted entity / table the code references or a migration creates.
    Entity {
        /// The entity / table name.
        name: String,
    },
    /// A configuration / environment key one agent reads and another provides.
    ConfigKey {
        /// The key name (e.g. `STRIPE_SECRET_KEY`).
        name: String,
    },
}

impl ArtifactKind {
    /// A short, stable human label for the kind (for verdict messages).
    pub fn kind_label(&self) -> &'static str {
        match self {
            ArtifactKind::Endpoint { .. } => "endpoint",
            ArtifactKind::Type { .. } => "type",
            ArtifactKind::Event { .. } => "event",
            ArtifactKind::Entity { .. } => "entity",
            ArtifactKind::ConfigKey { .. } => "config-key",
        }
    }

    /// The identity key two records must share to be "the same artifact" for
    /// reconciliation. Endpoints match on method+path; the rest on kind+name.
    /// Deterministic and case-preserving where the extractor already normalized.
    pub fn identity(&self) -> String {
        match self {
            ArtifactKind::Endpoint { method, path } => {
                format!("endpoint {} {}", method.to_uppercase(), path)
            }
            ArtifactKind::Type { name } => format!("type {name}"),
            ArtifactKind::Event { name } => format!("event {name}"),
            ArtifactKind::Entity { name } => format!("entity {name}"),
            ArtifactKind::ConfigKey { name } => format!("config-key {name}"),
        }
    }
}

/// The request/response shape carried on an endpoint, when the extractor could
/// recover it. A stack that cannot recover a shape (or a seam where the shape is
/// not statically knowable) leaves this `None` — the engine then reconciles on
/// method+path alone and reports shape-drift only when BOTH sides carry a shape.
///
/// The shape is a normalized, order-independent set of field names (not full
/// types) — enough to catch the classic `POST /members/export` vs `csv`, plus a
/// consumer that reads `memberId` off a `member_id` response. Full typed schema
/// is a staged, richer extractor (see the ADR).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    /// Request-body field names (normalized casing left to the extractor).
    pub request_fields: Vec<String>,
    /// Response-body field names.
    pub response_fields: Vec<String>,
    /// The status codes the artifact declares it can return.
    pub status_codes: Vec<u16>,
}

impl Shape {
    /// True when this shape carries no recoverable information (the extractor
    /// found method+path but no body/status detail). Such a shape is not
    /// compared: absence of evidence is never a drift finding.
    pub fn is_empty(&self) -> bool {
        self.request_fields.is_empty()
            && self.response_fields.is_empty()
            && self.status_codes.is_empty()
    }
}

/// A PRODUCED artifact: something an agent's repo EXPOSES for others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Produced {
    /// The `owner/repo` (or logical agent name) that produced it.
    pub repo: String,
    /// What was produced.
    pub kind: ArtifactKind,
    /// The recovered shape, if any (endpoints only, best-effort).
    #[serde(default)]
    pub shape: Option<Shape>,
    /// True when the producer ENFORCES an auth/permission guard on this artifact
    /// (an endpoint behind an auth middleware / guard). Only meaningful for
    /// endpoints; the auth-seam rule reads it. `None` = the extractor could not
    /// determine guard status (reported review-tier by the auth-seam rule).
    #[serde(default)]
    pub guarded: Option<bool>,
    /// Source location `path:line` for the verdict message (best-effort).
    #[serde(default)]
    pub location: String,
}

/// A CONSUMED usage: something an agent's repo DEPENDS ON from another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consumed {
    /// The `owner/repo` (or logical agent name) that consumed it.
    pub repo: String,
    /// What was consumed.
    pub kind: ArtifactKind,
    /// The shape the consumer EXPECTS, if recoverable.
    #[serde(default)]
    pub shape: Option<Shape>,
    /// True when this consumption is a UI affordance the UI GATES on a permission
    /// (a button shown only to authorized users). The auth-seam rule fires ONLY
    /// for these — a call the UI does not gate is out of scope, never a false
    /// positive. `None`/`false` = not a gated affordance.
    #[serde(default)]
    pub ui_gated: Option<bool>,
    /// Source location `path:line`.
    #[serde(default)]
    pub location: String,
}

/// The pair of lists a single repo's extractor emits: everything it PRODUCES and
/// everything it CONSUMES across the seam. The engine concatenates these across
/// all in-scope repos before reconciling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoArtifacts {
    /// The `owner/repo` this belongs to.
    pub repo: String,
    /// Everything this repo exposes.
    pub produced: Vec<Produced>,
    /// Everything this repo depends on.
    pub consumed: Vec<Consumed>,
}

/// Normalize a route path so semantically-identical routes across stacks compare
/// equal:
/// - ensures a single leading slash,
/// - strips a trailing slash (except the root `/`),
/// - collapses any path parameter segment (`:id`, `{id}`, `<id>`, `[id]`,
///   `$id`) to the neutral placeholder `{}`, so `/users/:id` (express),
///   `/users/{id}` (openapi/axum), and `/users/<id>` (flask) all normalize to
///   `/users/{}`.
///
/// This is the one place a route string is canonicalized; both producers and
/// consumers pass through it, so the engine compares apples to apples. It is
/// stack-AGNOSTIC: it recognizes the common param spellings but bakes in no
/// framework. Casing of literal segments is PRESERVED (a `/Members` vs
/// `/members` disagreement is a real drift, not something to paper over).
pub fn normalize_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    // Split off any query string; the seam is the path.
    let path = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let segments: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(canonical_segment)
        .collect();
    if segments.is_empty() {
        return "/".to_string();
    }
    format!("/{}", segments.join("/"))
}

/// Collapse one path segment to `{}` if it is a parameter in any common spelling,
/// else return it verbatim. Recognized param forms:
/// - explicit route params: `:name`, `{name}`, `<name>`, `[name]`, `$name`;
/// - a template interpolation a client wrote: `${...}`, `#{...}`, `%{...}`;
/// - a bare CONCRETE id a client substituted for a param (a pure-numeric or
///   uuid-like segment). A client call `GET /users/1` targets a producer's
///   `GET /users/:id`, so the concrete value must collapse to the placeholder to
///   reconcile. A literal WORD segment (`/users/me`) is NOT a param — it stays.
///
/// Everything else is a literal segment (casing preserved).
fn canonical_segment(seg: &str) -> String {
    let explicit_param = seg.starts_with(':')
        || seg.starts_with('$')
        || (seg.starts_with('{') && seg.ends_with('}'))
        || (seg.starts_with('<') && seg.ends_with('>'))
        || (seg.starts_with('[') && seg.ends_with(']'));
    // Template interpolations the consumer wrote into the path string.
    let interpolation = seg.contains("${")
        || seg.contains("#{")
        || seg.contains("%{")
        || seg.contains("{{");
    if explicit_param || interpolation || is_concrete_id(seg) {
        "{}".to_string()
    } else {
        seg.to_string()
    }
}

/// True when a segment is a CONCRETE id a client likely substituted for a route
/// param: all-digits, or a UUID (hex + hyphens). Deliberately conservative — a
/// mixed word like `user-42` or a slug stays literal, so only unambiguous id
/// values collapse.
fn is_concrete_id(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    // Pure numeric.
    if seg.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // UUID-shaped: hex digits + hyphens, at least one hyphen, and 32+ hex chars.
    let hex_or_hyphen = seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let hex_count = seg.chars().filter(|c| c.is_ascii_hexdigit()).count();
    hex_or_hyphen && seg.contains('-') && hex_count >= 32
}

/// Normalize a field name to a casing-insensitive comparison key so a
/// `member_id` producer and a `memberId` consumer are recognized as the SAME
/// field for the "did the consumer read a field the producer emits?" question,
/// while a genuinely different field (`memberId` vs `userId`) still differs.
///
/// The normalization strips separators (`_`, `-`, spaces) and lower-cases, so
/// `member_id` / `memberId` / `MemberID` / `member-id` all collapse to `memberid`.
/// Casing DRIFT itself (snake vs camel across the wire) is a separate,
/// convention-coherence concern the extractor can surface; this key is for
/// presence-matching.
pub fn normalize_field(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_' && *c != '-' && *c != ' ')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_collapses_param_spellings() {
        assert_eq!(normalize_path("/users/:id"), "/users/{}");
        assert_eq!(normalize_path("/users/{id}"), "/users/{}");
        assert_eq!(normalize_path("/users/<id>"), "/users/{}");
        assert_eq!(normalize_path("/users/[id]"), "/users/{}");
        assert_eq!(normalize_path("/users/$id"), "/users/{}");
    }

    #[test]
    fn normalize_path_handles_slashes_and_query() {
        assert_eq!(normalize_path("users/"), "/users");
        assert_eq!(normalize_path("/users//x/"), "/users/x");
        assert_eq!(normalize_path("/users?q=1"), "/users");
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn normalize_path_collapses_concrete_ids_and_interpolations() {
        // A client's concrete id collapses to match the producer's param route.
        assert_eq!(normalize_path("/users/1"), "/users/{}");
        assert_eq!(normalize_path("/users/${id}"), "/users/{}");
        assert_eq!(
            normalize_path("/users/550e8400-e29b-41d4-a716-446655440000"),
            "/users/{}"
        );
        // A literal word segment is NOT an id — it stays.
        assert_eq!(normalize_path("/users/me"), "/users/me");
        // A slug with a number is ambiguous → stays literal (conservative).
        assert_eq!(normalize_path("/posts/hello-42"), "/posts/hello-42");
    }

    #[test]
    fn normalize_path_preserves_literal_casing() {
        // Casing of a literal segment is a real drift signal — do NOT collapse it.
        assert_ne!(normalize_path("/Members"), normalize_path("/members"));
    }

    #[test]
    fn endpoint_identity_is_method_and_path() {
        let a = ArtifactKind::Endpoint {
            method: "post".into(),
            path: "/members/export".into(),
        };
        let b = ArtifactKind::Endpoint {
            method: "POST".into(),
            path: "/members/export".into(),
        };
        assert_eq!(a.identity(), b.identity(), "method casing is normalized");
    }

    #[test]
    fn normalize_field_collapses_separators_and_casing() {
        assert_eq!(normalize_field("member_id"), normalize_field("memberId"));
        assert_eq!(normalize_field("member-id"), normalize_field("MemberID"));
        assert_ne!(normalize_field("memberId"), normalize_field("userId"));
    }

    #[test]
    fn empty_shape_is_detected() {
        assert!(Shape::default().is_empty());
        let s = Shape {
            response_fields: vec!["id".into()],
            ..Default::default()
        };
        assert!(!s.is_empty());
    }
}
