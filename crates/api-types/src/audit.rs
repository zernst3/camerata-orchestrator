//! The five-kind finding taxonomy (Pass-1 W3, requirement A5.6 of
//! the audit findings tracker).
//!
//! The post-inspection table was built on a binary premise: a row is a violation or it is
//! not. That premise no longer holds — an inspection now returns true violations, consistency
//! deviations, suggested conventions with ZERO violations, "no convention adopted"
//! observations, and honesty rows for rules that could not be evaluated at all.
//! [`FindingKind`] is that taxonomy as a FIRST-CLASS structured field on every finding row,
//! populated by whatever produced the row — NEVER derived downstream by scanning model prose
//! (the Fable review's hard constraint: structured identity, never keyword-list adjustment).
//!
//! ## Wire shape
//!
//! The enum is internally tagged on `"kind"` and designed to be `#[serde(flatten)]`ed into a
//! findings row, so the exported JSON reads:
//!
//! ```json
//! { "...row fields...", "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
//!   "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints" }
//! ```
//!
//! Every variant carries everything a consumer needs WITHOUT re-deriving it from prose: a
//! `Deviation` row alone is enough to render "all but 2 of your 47 assessed sites follow
//! <convention>; these 2 deviate" — numerator, denominator, and the observed majority
//! convention are structured fields, never parsed out of `detail`.
//!
//! ## Back-compat / the honesty rule for absent kinds
//!
//! The owning row field is `Option<FindingKind>` with `#[serde(default)]` so reports
//! persisted before this field existed still deserialize. `None` is NOT a synonym for
//! `Defect`: an exporter must surface it as its own explicit wire value
//! ([`UNSPECIFIED_KIND_TAG`]) so a legacy/unmapped emission path is VISIBLE as a gap rather
//! than silently counted as a violation (the tracker's backfill rule: nothing may default
//! silently to `Defect`).

use serde::{Deserialize, Serialize};

/// The wire tag an exporter uses for a row whose producer never assigned a [`FindingKind`]
/// (a report persisted before the taxonomy existed, or an emission path missed by the
/// backfill — the latter is a bug to surface, not to paper over). Deliberately NOT a
/// [`FindingKind`] variant: no producer may ever emit it, it exists only as the exporters'
/// honest rendering of an absent kind.
pub const UNSPECIFIED_KIND_TAG: &str = "unspecified";

/// What a finding row IS — the five-kind taxonomy (A5.6). See the module doc comment.
///
/// Internally tagged on `"kind"` (snake_case tags: `defect` / `deviation` /
/// `convention_observed` / `no_convention` / `not_assessed`) so a flattened row carries one
/// flat `"kind"` discriminator plus the variant's own payload fields as siblings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingKind {
    /// An objective fault. Arises from a `kind = "defect"` rule, from a `decision` /
    /// `default_with_exceptions` rule whose decided option is flagged `violates` (the
    /// repo was judged to follow the rule's own named degenerate form), or from any
    /// deterministic producer whose whole vocabulary is faults (the content-rule floor,
    /// external-tool previews, the dependency audit, the architectural checkers, the
    /// model's advisory pass). Severity is unconstrained apart from the rule's
    /// `max_severity` ceiling (E2). Carries no payload of its own: provenance (which
    /// producer) is already a structured axis on the row (`preview_tool`, citation tier),
    /// and duplicating it here would create two sources of truth.
    Defect,
    /// A consistency violation against the convention the codebase itself predominantly
    /// follows — "all but 2 of your API endpoints do NOT use API versioning, therefore you
    /// have 2 consistency violations." One row per deviating site; every row for the same
    /// rule carries the same group-level numbers (see [`DeviationKind`]).
    Deviation(DeviationKind),
    /// The codebase consistently follows a convention and there are ZERO violations.
    /// Informational — and it SHIPS in the product (A5.5): the audit derives the decisions,
    /// governance later enforces them. One row per rule.
    ConventionObserved(ConventionObservedKind),
    /// No convention has been adopted: the codebase is inconsistent with no clear majority.
    /// Per A5.3 this is ONE informational row per rule, never N violations — the emission
    /// pipeline severs the per-site violation path for such a rule and synthesizes exactly
    /// one of these from the per-rule recommendation record (see
    /// `camerata_server::ai_audit::audit_repo`).
    NoConvention(NoConventionKind),
    /// The rule was armed but could not be evaluated (tool failed, no files matched, the
    /// model declined). An honesty row, distinguishable from "assessed and clean".
    NotAssessed(NotAssessedKind),
}

impl FindingKind {
    /// The stable wire tag for this kind — exactly the internally-tagged serde
    /// discriminator, exposed so renderers (xlsx cells, PDF labels) never re-derive it.
    pub fn tag(&self) -> &'static str {
        match self {
            FindingKind::Defect => "defect",
            FindingKind::Deviation(_) => "deviation",
            FindingKind::ConventionObserved(_) => "convention_observed",
            FindingKind::NoConvention(_) => "no_convention",
            FindingKind::NotAssessed(_) => "not_assessed",
        }
    }

    /// The wire tag for an `Option<FindingKind>` as the exporters render it: the kind's own
    /// tag, or [`UNSPECIFIED_KIND_TAG`] when the producer never assigned one. Centralized so
    /// "absent is surfaced, never silently `defect`" is a single shared decision.
    pub fn tag_or_unspecified(kind: Option<&FindingKind>) -> &'static str {
        kind.map(FindingKind::tag).unwrap_or(UNSPECIFIED_KIND_TAG)
    }

    /// Whether this kind is an INFORMATIONAL observation (convention observed / no
    /// convention / not assessed) rather than something wrong at a site (defect/deviation).
    /// Informational kinds always ship at `info` severity and live outside the action tiers.
    pub fn is_informational(&self) -> bool {
        matches!(
            self,
            FindingKind::ConventionObserved(_)
                | FindingKind::NoConvention(_)
                | FindingKind::NotAssessed(_)
        )
    }
}

/// WHO can fix a finding — the W33 "ownership" axis, orthogonal to `kind` (what it is),
/// `severity` (how bad), and the disposition (the triage call). It answers: is the fix in the
/// client's OWN first-party code, or does it live somewhere they do not control? A non-
/// `ClientOwned` finding is real and keeps its true severity, but it is routed out of the action
/// tiers into a "Findings outside your direct control" section and excluded from the remediation
/// budget — ranking an unfixable finding beside an actionable one spends the client's attention
/// on something they cannot act on. Derived per finding (see `camerata_server::ownership`), never
/// hand-authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    /// The fix is in the client's own code — the normal, actionable case (today's default for
    /// every finding). Stays in the action tiers and the remediation budget.
    #[default]
    ClientOwned,
    /// A platform-managed resource the client cannot change (e.g. Supabase `auth.*`/`storage.*`
    /// RLS). Action: accept the risk or escalate to the platform.
    PlatformManaged,
    /// A third-party dependency with NO patched version available yet (a dependency WITH a fix
    /// stays `ClientOwned` — upgrading is the fix). Action: monitor the advisory, accept, or
    /// replace the dependency.
    ExternalDependency,
    /// Generated output — the fix belongs in the generator/source, not the artifact.
    Generated,
    /// Vendored third-party code — the fix belongs upstream.
    Vendored,
}

impl Ownership {
    /// The stable snake_case wire tag, for exporters that render a cell/column rather than
    /// serde (the xlsx, the CSV header value).
    pub fn tag(&self) -> &'static str {
        match self {
            Ownership::ClientOwned => "client_owned",
            Ownership::PlatformManaged => "platform_managed",
            Ownership::ExternalDependency => "external_dependency",
            Ownership::Generated => "generated",
            Ownership::Vendored => "vendored",
        }
    }

    /// Whether the client can fix this in their own code. Only `ClientOwned` is directly
    /// fixable; every other ownership routes to the "outside your control" section and out of
    /// the remediation budget.
    pub fn is_client_fixable(&self) -> bool {
        matches!(self, Ownership::ClientOwned)
    }
}

/// The exporters' ALWAYS-PRESENT wire rendering of an `Option<FindingKind>` (Pass-1 W3,
/// A5.6's backfill rule): a known kind serializes exactly as the kind itself (internally
/// tagged, flat payload), while an ABSENT kind serializes as `{"kind": "unspecified"}` — the
/// `kind` key is therefore present on EVERY exported row, so a consumer can never confuse "a
/// legacy/unmapped row" with "a defect" (or with a row whose producer simply forgot the
/// field). Designed to be `#[serde(flatten)]`ed into an export row struct; serialize-only by
/// design (exports are write-only artifacts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindOnWire(pub Option<FindingKind>);

impl KindOnWire {
    /// The row's wire tag — the kind's own tag, or [`UNSPECIFIED_KIND_TAG`].
    pub fn tag(&self) -> &'static str {
        FindingKind::tag_or_unspecified(self.0.as_ref())
    }
}

impl Serialize for KindOnWire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Some(kind) => kind.serialize(serializer),
            None => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("kind", UNSPECIFIED_KIND_TAG)?;
                map.end()
            }
        }
    }
}

/// Payload of [`FindingKind::Deviation`] — the structured sentence. All counts are
/// GROUP-level (per rule, across the whole scan), repeated verbatim on every deviating row
/// for that rule, so any single row suffices to render "N of M sites deviate from
/// <convention>" without joining rows or parsing prose.
///
/// Field-name note: these flatten into the finding row's own JSON object, so every name here
/// must stay disjoint from the row's field names (`severity`, `detail`, `snippet`, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviationKind {
    /// NUMERATOR: how many sites deviate from the observed convention for this rule, across
    /// the whole scan — computed structurally by the pipeline (rows under the rule plus
    /// each row's absorbed merge sites), never model-counted.
    pub deviating_sites: usize,
    /// DENOMINATOR: how many sites this rule's decision applies to in total (deviating +
    /// conforming), when the producer could establish it — the recommendation pass reports
    /// it as a structured field. `None` means the producer could not count honestly;
    /// renderers then show the numerator alone rather than fabricating a denominator.
    pub assessed_sites: Option<usize>,
    /// The observed majority convention's option id on the rule (structured identity into
    /// the corpus), when the convention corresponds to an authored option. `None` only for
    /// a producer that established a majority convention outside the authored option set.
    pub convention_option_id: Option<String>,
    /// The observed majority convention's human label, verbatim from the AUTHORED corpus
    /// option (never model prose) whenever `convention_option_id` resolves.
    pub convention_label: String,
}

/// Payload of [`FindingKind::ConventionObserved`] — the convention the codebase was judged
/// to consistently follow, as structured corpus identity (A5.5: this set ships in the
/// product as the derived decision set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConventionObservedKind {
    /// The authored option id the codebase follows.
    pub convention_option_id: String,
    /// The authored option's human label (corpus text, never model prose).
    pub convention_label: String,
}

/// One authored option's observed usage inside a [`NoConventionKind`] row — which options
/// the codebase was seen using, and (when the producer counted) at how many sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedOptionUsage {
    /// The authored option id (validated against the rule's real options by the parser —
    /// a usage naming a non-existent option is dropped at parse, never carried).
    pub option_id: String,
    /// The authored option's human label.
    pub label: String,
    /// How many sites were seen using this option, when the producer reported a count as a
    /// structured field. `None` = observed but not counted.
    pub sites: Option<usize>,
}

/// Payload of [`FindingKind::NoConvention`] — the observed split. May be empty when the
/// producer established "no clear majority" without a per-option breakdown; the row is
/// still honest (the `detail` prose explains), just not enumerated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoConventionKind {
    /// The authored options the codebase was observed using, with per-option site counts
    /// when available.
    pub observed_options: Vec<ObservedOptionUsage>,
}

/// Why a [`FindingKind::NotAssessed`] rule could not be evaluated. A closed, structured
/// vocabulary — never free text — so consumers can filter/aggregate without prose-matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotAssessedReason {
    /// The pass that would have evaluated this rule failed or timed out.
    PassFailed,
    /// The rule was armed but no file in the scan matched its scope, so nothing was ever
    /// judged against it.
    NoFilesMatched,
    /// An external tool this rule depends on could not run.
    ToolFailed,
    /// The model declined/omitted a judgment for this rule.
    ModelDeclined,
}

impl NotAssessedReason {
    /// Stable wire string (the serde snake_case tag), for renderers.
    pub fn as_str(self) -> &'static str {
        match self {
            NotAssessedReason::PassFailed => "pass_failed",
            NotAssessedReason::NoFilesMatched => "no_files_matched",
            NotAssessedReason::ToolFailed => "tool_failed",
            NotAssessedReason::ModelDeclined => "model_declined",
        }
    }
}

/// Payload of [`FindingKind::NotAssessed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotAssessedKind {
    /// The structured reason class — see [`NotAssessedReason`].
    pub reason: NotAssessedReason,
    /// The producer's verbatim detail for the failure (an error message, the unmatched
    /// scope), carried for the human reader. Named `reason_detail` (not `detail`) because
    /// this payload flattens into a row that already has its own `detail` field.
    pub reason_detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// req A5.6: the five kinds serialize to the documented flat, internally-tagged wire
    /// shape — one `"kind"` discriminator plus the payload fields as siblings, nothing
    /// nested, nothing prose-encoded.
    #[test]
    fn req_a5_6_kind_serializes_internally_tagged_with_flat_payload() {
        let k = FindingKind::Deviation(DeviationKind {
            deviating_sites: 2,
            assessed_sites: Some(47),
            convention_option_id: Some("versioned".to_string()),
            convention_label: "URI-versioned endpoints".to_string(),
        });
        let v = serde_json::to_value(&k).unwrap();
        assert_eq!(v["kind"], "deviation");
        assert_eq!(v["deviating_sites"], 2);
        assert_eq!(v["assessed_sites"], 47);
        assert_eq!(v["convention_option_id"], "versioned");
        assert_eq!(v["convention_label"], "URI-versioned endpoints");

        let d = serde_json::to_value(FindingKind::Defect).unwrap();
        assert_eq!(d["kind"], "defect");

        let na = serde_json::to_value(FindingKind::NotAssessed(NotAssessedKind {
            reason: NotAssessedReason::NoFilesMatched,
            reason_detail: "no .tsx files in scan scope".to_string(),
        }))
        .unwrap();
        assert_eq!(na["kind"], "not_assessed");
        assert_eq!(na["reason"], "no_files_matched");
    }

    /// req A5.6: a Deviation row alone carries numerator, denominator, and the observed
    /// majority convention — round-tripped losslessly, so the owner's canonical sentence
    /// ("all but 2 of your 47 endpoints…") is renderable from structured fields only.
    #[test]
    fn req_a5_6_deviation_round_trips_numerator_denominator_and_convention() {
        let k = FindingKind::Deviation(DeviationKind {
            deviating_sites: 2,
            assessed_sites: Some(47),
            convention_option_id: Some("versioned".to_string()),
            convention_label: "URI-versioned endpoints".to_string(),
        });
        let json = serde_json::to_string(&k).unwrap();
        let back: FindingKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, k);
        // An honest unknown denominator stays None rather than becoming 0/fabricated.
        let unknown = FindingKind::Deviation(DeviationKind {
            deviating_sites: 3,
            assessed_sites: None,
            convention_option_id: None,
            convention_label: "observed majority".to_string(),
        });
        let back2: FindingKind =
            serde_json::from_str(&serde_json::to_string(&unknown).unwrap()).unwrap();
        assert_eq!(back2, unknown);
    }

    /// req A5.6 backfill rule: an absent kind is surfaced as its own explicit
    /// `"unspecified"` tag — NEVER silently rendered as `"defect"`.
    #[test]
    fn req_a5_6_absent_kind_tags_unspecified_never_defect() {
        assert_eq!(FindingKind::tag_or_unspecified(None), "unspecified");
        assert_ne!(FindingKind::tag_or_unspecified(None), "defect");
        assert_eq!(
            FindingKind::tag_or_unspecified(Some(&FindingKind::Defect)),
            "defect"
        );
    }

    /// req A5.3 / A5.5: the informational classification is a property of the KIND itself
    /// (structured), not of severity strings or prose.
    #[test]
    fn req_a5_6_informational_split_is_structural() {
        assert!(!FindingKind::Defect.is_informational());
        assert!(!FindingKind::Deviation(DeviationKind {
            deviating_sites: 1,
            assessed_sites: None,
            convention_option_id: None,
            convention_label: String::new(),
        })
        .is_informational());
        assert!(FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: "a".to_string(),
            convention_label: "A".to_string(),
        })
        .is_informational());
        assert!(FindingKind::NoConvention(NoConventionKind {
            observed_options: vec![],
        })
        .is_informational());
        assert!(FindingKind::NotAssessed(NotAssessedKind {
            reason: NotAssessedReason::PassFailed,
            reason_detail: String::new(),
        })
        .is_informational());
    }

    /// req A5.6 back-compat contract: the owning-row pattern is
    /// `#[serde(default, flatten)] kind: Option<FindingKind>`. This test pins the three
    /// serde behaviors `camerata_server::onboard::Finding` depends on: (1) legacy JSON with
    /// NO `kind` key deserializes to `None` (old persisted reports still load); (2) a
    /// present kind round-trips losslessly through the flattened shape next to the row's
    /// own fields; (3) `None` serializes to no `kind` key at all.
    #[test]
    fn req_a5_6_option_kind_flattens_and_legacy_rows_deserialize_as_none() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Row {
            severity: String,
            detail: String,
            line: usize,
            #[serde(default, flatten)]
            kind: Option<FindingKind>,
        }
        // (1) legacy row: no kind key anywhere.
        let legacy: Row =
            serde_json::from_str(r#"{"severity":"high","detail":"d","line":3}"#).unwrap();
        assert_eq!(legacy.kind, None);
        // (2) full round-trip with a payload-carrying variant, next to real row fields.
        let row = Row {
            severity: "medium".to_string(),
            detail: "2 deviating endpoints".to_string(),
            line: 42,
            kind: Some(FindingKind::Deviation(DeviationKind {
                deviating_sites: 2,
                assessed_sites: Some(47),
                convention_option_id: Some("versioned".to_string()),
                convention_label: "URI-versioned endpoints".to_string(),
            })),
        };
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(
            v["kind"], "deviation",
            "flat discriminator on the row itself"
        );
        assert_eq!(v["deviating_sites"], 2);
        assert_eq!(
            v["detail"], "2 deviating endpoints",
            "row's own field untouched"
        );
        let back: Row = serde_json::from_value(v).unwrap();
        assert_eq!(back, row);
        // (2b) a legacy row with a STRAY unknown key and no `kind` tag: the flatten
        // remainder is non-empty, which is the one shape where `Option<FindingKind>` could
        // plausibly error instead of yielding None — pinned here so the behavior is a
        // conscious contract, not an accident. (Load paths that deserialize persisted
        // reports are fail-soft `.ok()` readers, but the contract still matters.)
        let stray: Result<Row, _> =
            serde_json::from_str(r#"{"severity":"high","detail":"d","line":3,"bogus":true}"#);
        match stray {
            Ok(row) => assert_eq!(row.kind, None, "stray keys must not conjure a kind"),
            Err(e) => panic!(
                "a legacy row with an unknown sibling key must still deserialize (kind=None), \
                 got: {e}"
            ),
        }
        // (3) None emits no kind key.
        let none_row = Row {
            severity: "low".to_string(),
            detail: String::new(),
            line: 0,
            kind: None,
        };
        let v = serde_json::to_value(&none_row).unwrap();
        assert!(v.get("kind").is_none());
    }

    /// req A5.6 export contract: `KindOnWire` keeps the `kind` key present on EVERY row —
    /// a known kind flattens as itself; an absent kind flattens as the explicit
    /// `"unspecified"` tag (never silently missing, never `"defect"`).
    #[test]
    fn req_a5_6_kind_on_wire_always_emits_a_kind_key() {
        #[derive(Serialize)]
        struct Row {
            severity: String,
            #[serde(flatten)]
            kind: KindOnWire,
        }
        let known = serde_json::to_value(Row {
            severity: "high".to_string(),
            kind: KindOnWire(Some(FindingKind::Deviation(DeviationKind {
                deviating_sites: 2,
                assessed_sites: Some(47),
                convention_option_id: Some("versioned".to_string()),
                convention_label: "URI-versioned endpoints".to_string(),
            }))),
        })
        .unwrap();
        assert_eq!(known["kind"], "deviation");
        assert_eq!(known["deviating_sites"], 2);

        let legacy = serde_json::to_value(Row {
            severity: "high".to_string(),
            kind: KindOnWire(None),
        })
        .unwrap();
        assert_eq!(
            legacy["kind"], "unspecified",
            "absent kind must be explicit on the wire"
        );
        assert_ne!(legacy["kind"], "defect");
    }

    /// The tags are the serde discriminators — `tag()` can never drift from the wire.
    #[test]
    fn req_a5_6_tag_matches_serde_discriminator_for_every_variant() {
        for k in [
            FindingKind::Defect,
            FindingKind::Deviation(DeviationKind {
                deviating_sites: 1,
                assessed_sites: None,
                convention_option_id: None,
                convention_label: String::new(),
            }),
            FindingKind::ConventionObserved(ConventionObservedKind {
                convention_option_id: "x".to_string(),
                convention_label: "X".to_string(),
            }),
            FindingKind::NoConvention(NoConventionKind {
                observed_options: vec![ObservedOptionUsage {
                    option_id: "x".to_string(),
                    label: "X".to_string(),
                    sites: Some(3),
                }],
            }),
            FindingKind::NotAssessed(NotAssessedKind {
                reason: NotAssessedReason::ToolFailed,
                reason_detail: "eslint exited 2".to_string(),
            }),
        ] {
            let v = serde_json::to_value(&k).unwrap();
            assert_eq!(v["kind"], k.tag());
            let back: FindingKind = serde_json::from_value(v).unwrap();
            assert_eq!(back, k);
        }
    }
}
