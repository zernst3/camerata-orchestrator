//! Corpus source freshness, Part A of the corpus freshness routine
//! (`_workspace/brownfield-audits/CORPUS_FRESHNESS_WORK_ORDER.md`).
//!
//! Every `[[sources]]` URL in every rule is fetched politely and judged ok / redirected / dead (or
//! unreachable when the network gave no answer). When a rule cites an anchor, that section's text
//! is normalised, hashed and compared with the hash from the last run, which lives in a sidecar
//! file ([`sidecar_path`]) and never in a rule. The result is a date-stamped report
//! ([`render_text`] / [`render_json`]).
//!
//! Nothing here edits a rule, and nothing here calls a model. This is the deterministic half; the
//! quarterly judgment pass (Part B) reads this report as its input.
//!
//! Not wired into any export yet. [`source_confidence`] is the function an export calls per rule
//! when that wiring is built.

mod check;
mod diff;
mod export;
mod fetch;
mod render;
mod section;
mod sidecar;

pub use check::{
    run_check, CheckConfig, Counts, LinkFinding, MissingAnchor, RuleRef, SectionChange,
    SourceCheckReport,
};
pub use diff::{diff_sections, DiffLine, DiffOp};
pub use export::{source_confidence, SourceConfidence, DEFAULT_CHANGED_WINDOW_DAYS};
pub use fetch::{FetchOutcome, Fetcher, HttpFetcher};
pub use render::{render_json, render_text};
pub use section::{
    extract_section, hash_text, normalise_whitespace, split_fragment, SectionLookup,
};
pub use sidecar::{
    cap_excerpt, sidecar_path, LinkState, SourceHashes, SourceRecord, EXCERPT_CAP_CHARS,
    SIDECAR_SCHEMA_VERSION,
};
