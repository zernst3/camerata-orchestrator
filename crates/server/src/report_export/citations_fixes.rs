//! Citation resolution, AI-tier classification, fix resolution and remediation
//! text instantiation.

use super::*;

/// FIX 7 (2026-09-13 review) — one FACTUAL, non-fear-selling sentence of what a rule's
/// exposure MEANS in plain business terms, authored per `rule_id`. Deliberately a small Rust
/// map, NOT a corpus TOML field: the corpus-wide `remediation` authoring pass (FIX 1, Phase
/// 2a) runs in parallel against these same TOML files, and folding a second authored-prose
/// field into that same parallel-editable surface risks a merge collision over the same lines
/// for no benefit — this is a small, independently-owned map instead. Covers the Supabase
/// pack (the corpus currently backing the do-now items the flagship sample exercises) plus the
/// handful of universal/deterministic-floor rules most likely to land in "do now" on a typical
/// scan. Returns `None` for anything not yet authored — the template must omit the line
/// gracefully in that case, never fabricate one. NOTE: this could migrate to a corpus `impact`
/// field alongside `remediation` once the two authoring passes are no longer running in
/// parallel; deliberately not done in this pass.
pub(crate) fn business_impact_for_rule(rule_id: &str) -> Option<&'static str> {
    match rule_id {
        // ── Supabase: RLS ──────────────────────────────────────────────────────────
        "SUPABASE-RLS-ENABLED-1" => Some(
            "No RLS on this table means every row is readable and writable by anyone holding \
             the public anon key, with no login required.",
        ),
        "SUPABASE-RLS-NO-POLICY-1" => Some(
            "RLS enabled with zero policies denies all access by default, which typically \
             breaks the feature for real users rather than exposing data, and often gets \
             patched with an overly permissive policy under deadline pressure.",
        ),
        "SUPABASE-RLS-POLICY-DISABLED-1" => Some(
            "A policy exists but RLS itself is off, so the policy is decorative: every row is \
             exposed exactly as if no policy had ever been written.",
        ),
        "SUPABASE-RLS-PERMISSIVE-TRUE-1" => Some(
            "A policy that always evaluates to true grants every anon or authenticated caller \
             full access to the table, which is functionally the same exposure as having no \
             RLS at all.",
        ),
        "SUPABASE-RLS-USER-METADATA-1" => Some(
            "Authorizing against user-editable metadata lets any authenticated user grant \
             themselves elevated access by editing their own profile fields.",
        ),
        "SUPABASE-RLS-VIEW-INVOKER-1" => Some(
            "A view without security_invoker runs with the view creator's privileges, so it \
             can silently bypass the Row Level Security policies protecting the underlying \
             tables.",
        ),
        "SUPABASE-RLS-INITPLAN-1" => Some(
            "This is a performance defect, not an access hole: unwrapped auth calls \
             re-evaluate once per row and can make an otherwise-correct RLS policy time out \
             under real load.",
        ),
        // ── Supabase: auth ─────────────────────────────────────────────────────────
        "SUPABASE-AUTH-EDGE-JWT-1" => Some(
            "An edge function reachable with no authentication check can be invoked by anyone \
             on the internet, including for state-changing operations like a payment.",
        ),
        "SUPABASE-AUTH-GETSESSION-SERVER-1" => Some(
            "Trusting an unverified session cookie on the server lets a forged cookie \
             impersonate any user at that code path.",
        ),
        "SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1" => Some(
            "A service-role client bypasses Row Level Security entirely, so a request handler \
             using it without its own authorization check grants full database access to \
             whoever can reach that endpoint.",
        ),
        "SUPABASE-AUTH-USERS-EXPOSED-1" => Some(
            "Exposing auth.users through a view or grant leaks every user's email, phone, and \
             identity metadata to anyone who can query it.",
        ),
        // ── Supabase: secrets, storage, exposure, functions ───────────────────────────
        "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1" => Some(
            "A service-role key shipped to the browser bypasses Row Level Security and grants \
             full read and write access to every table to anyone who opens developer tools.",
        ),
        "SUPABASE-STORAGE-PUBLIC-BUCKET-1" => Some(
            "A public bucket serves any object to anyone who has, or can guess, its URL, with \
             no login and no access policy involved.",
        ),
        "SUPABASE-STORAGE-OBJECT-POLICY-1" => Some(
            "Unscoped write or delete access on storage objects lets any caller overwrite or \
             delete files that belong to other users.",
        ),
        "SUPABASE-EXPOSURE-MATVIEW-1" => Some(
            "A materialized view cannot carry row-level policies, so exposing it in the API \
             makes it all-or-nothing: every row is visible to anyone who can query it.",
        ),
        "SUPABASE-EXPOSURE-SCHEMAS-1" => Some(
            "Every schema added to the exposed-schemas list becomes reachable over the API, \
             including any table in it that was never reviewed for Row Level Security.",
        ),
        "SUPABASE-FUNC-SEARCH-PATH-1" => Some(
            "A SECURITY DEFINER function without a fixed search_path can be hijacked into \
             running an attacker-created function with the function owner's elevated \
             privileges.",
        ),
        // ── Universal / deterministic-floor ────────────────────────────────────────
        "ARCH-NO-SECRETS-IN-URL-1" => Some(
            "A credential carried in a URL is written into proxy logs, browser history, and \
             server access logs by default, which is a durable exposure even after the \
             credential is rotated.",
        ),
        "SEC-NO-PRIVATE-KEY-1" => Some(
            "A private key committed to the repository is exposed to everyone with repository \
             access, past and future, and stays readable in git history even after the file \
             is deleted.",
        ),
        "SEC-NO-SECRET-FILE-1" => Some(
            "A committed secret-bearing file (a private key, keystore, or real .env) is \
             exposed to everyone with repository access, past and future.",
        ),
        "SEC-NO-HARDCODED-SECRETS-1" => Some(
            "A hardcoded credential in source is exposed to everyone with repository access, \
             and to anyone who ever receives a copy of the build.",
        ),
        "SEC-NO-DISABLED-TLS-1" => Some(
            "Disabled certificate verification lets any network position between the client \
             and server intercept or alter traffic without detection.",
        ),
        "SEC-NO-UNSAFE-DESERIALIZATION-1" => Some(
            "Deserializing untrusted input without safeguards can let an attacker execute \
             arbitrary code on the server simply by controlling the input.",
        ),
        "SEC-NO-RAW-SQL-CONCAT-1" => Some(
            "String-concatenated SQL lets an attacker who controls any part of the input \
             read, modify, or delete arbitrary rows in the database.",
        ),
        _ => None,
    }
}

// ── Citation join (§4.4) ─────────────────────────────────────────────────────────

/// Join `rule_id` (+ `preview_tool`, when set) against the loaded corpus to recover its
/// authoritative sources. Priority: (1) a corpus rule with non-empty `sources` is
/// "grounded" — cite them verbatim (CWE/OWASP/RFC/linter docs); (2) a deterministic
/// preview finding with no corpus citation is labeled by the REAL tool that enforces it
/// (still honest — just not corpus-documented); (3) anything else (a free-text/AI-tier
/// rule id the corpus never saw) is "AI-advisory, model-inferred." — never dressed up.
/// Item 3: a source counts as an external authority only when its `url` is a real, fetchable
/// external URL (CWE / OWASP / RFC / Supabase docs / a linter's own docs...). Camerata's own
/// internal scaffolding docs (`docs/ENFORCEMENT.md` §RULE_REGISTRY entries, which exist only in
/// Camerata-built repos, never in an arbitrary client's) are filtered out entirely rather than
/// rendered as if they were an authority a client could go verify.
pub(crate) fn is_external_source_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Join `rule_id` against `corpus`, falling back to the external-tool grounding maps
/// (`crate::external_tool_floor_category` — Semgrep's `semgrep_floor_category` plus
/// ESLint's `eslint_floor_category`) when `rule_id` is an external tool's OWN rule id
/// (e.g. `camerata.security.taint-sql-injection-python`, or `react-hooks/exhaustive-deps`)
/// rather than one of our corpus ids. This is the single place "which corpus TOML entry
/// supplies this finding's citation/remediation/effort" is decided, so `resolve_citation`,
/// `resolve_fix`, and `resolve_effort` can't drift from each other on what "grounded"
/// means for a preview finding. The finding's OWN `rule_id` (and `preview_tool`) are never
/// touched by this — only which corpus rule's authored facts get joined in.
///
/// A direct hit (`rule_id` IS a corpus id) always wins over the grounding map, so this
/// is fully backward compatible with every existing call site that already passes a
/// corpus id directly (e.g. `resolve_citation("SEC-NO-RAW-SQL-CONCAT-1", ...)`).
pub(crate) fn corpus_rule_for<'c>(
    rule_id: &str,
    corpus: Option<&'c camerata_rules::RuleSet>,
) -> Option<&'c camerata_rules::Rule> {
    corpus.and_then(|c| {
        c.get_by_id(rule_id).or_else(|| {
            crate::external_tool_floor_category(rule_id)
                .and_then(|grounded_id| c.get_by_id(grounded_id))
        })
    })
}

pub(crate) fn resolve_citation(
    rule_id: &str,
    preview_tool: Option<&str>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> CitationJson {
    if let Some(rule) = corpus_rule_for(rule_id, corpus) {
        let sources: Vec<CitationSourceJson> = rule
            .sources
            .iter()
            .filter(|s| is_external_source_url(&s.url))
            .map(|s| CitationSourceJson {
                title: s.title.clone(),
                url: s.url.clone(),
                linter: s.linter.clone(),
            })
            .collect();
        if !sources.is_empty() {
            let label = sources
                .iter()
                .map(|s| s.title.clone())
                .collect::<Vec<_>>()
                .join("; ");
            return CitationJson {
                kind: "grounded".to_string(),
                label,
                sources,
            };
        }
    }
    if let Some(tool) = preview_tool {
        // Client-facing fix (2026-10-07): this used to read "Deterministic preview, enforced
        // by {tool} ({rule_id}); not yet wired into the CI gate" — calling a tool-sourced row
        // "Deterministic" (a word this report otherwise reserves for Camerata's own proven
        // floor checks) in the SAME sentence that says nothing enforces it, plus internal
        // "wired into the CI gate" phrasing. The row's own tier IS "preview", named with its
        // tool, never "Deterministic" — see `classify_ai_finding`'s sibling fix and the
        // `xlsx_export::provenance` computation this label feeds.
        return CitationJson {
            kind: "preview".to_string(),
            label: format!("Preview check by {tool} ({rule_id}); not yet part of your CI pipeline"),
            sources: Vec::new(),
        };
    }
    CitationJson {
        kind: "advisory".to_string(),
        label: "AI-advisory, model-inferred.".to_string(),
        sources: Vec::new(),
    }
}

/// H5 (2026-10-28): the label prefix of a citation the P3 class fallback attached by KEYWORD
/// match (`classify_ai_finding`). The standard is real and relevant, but no Camerata rule
/// checked the code against it: the model reported a defect and its wording matched a defect
/// class. Such a citation carries kind `"related"`, never `"grounded"`, so neither the report,
/// the workbook nor `findings.json` presents it as a rule that was checked. Triage
/// confirmation was the other option and was rejected: triage is optional in the desktop
/// flow, so gating the citation on it would strip every keyword-matched finding of its
/// standard (and, through `is_uncited_ai_finding`, out of the curated set) in any export made
/// before a human triaged. See docs/TECHNICAL.md, "Related standards".
pub(crate) const RELATED_STANDARD_LABEL_PREFIX: &str =
    "Related standard (matched by keyword, not a rule check): ";

/// The Methodology sentence that explains [`RELATED_STANDARD_LABEL_PREFIX`] to a reader.
pub(crate) const RELATED_STANDARD_NOTE: &str = "A source marked \"related standard\" was matched \
    to a model finding by keyword and names the published standard the issue relates to, not a \
    rule Camerata checked the code against.";

// ── P3 (2026-09-29): the citation gate ──────────────────────────────────────────────
//
// Several of the report's MOST important findings — a cross-tenant RLS policy, RLS
// disabled on a payments table, a stored-XSS sink — are produced by the AI tier, which
// invents its own rule id (an `AI-`-prefixed id; see `is_ai_tier`) the corpus never saw.
// `resolve_citation` is honest about that: it labels such a finding "AI-advisory,
// model-inferred." rather than dressing it up. But the product promises every CURATED
// finding cites the published standard it breaks (the hardening plan's bar item 3) — a
// citation-less finding sitting in the curated set falsifies that promise, for exactly
// the findings a client is most likely to act on first.
//
// The fix is NOT to invent a bespoke Rust-side citation per finding (that just moves the
// "trust me" problem into this file instead of a TOML). Instead: classify the AI finding
// into one of a small, closed set of well-known defect CLASSES by its rule id tokens +
// `category` + detail-text keywords (never a fixture-specific string — see
// `classify_ai_finding`'s doc comment), then reuse an EXISTING grounded corpus rule's own
// citation for that class. Where a class had no corpus rule yet, one was added under
// `crates/rules/principles/universal/sec-no-*-1.toml` rather than inventing a citation in
// Rust — grounding always means a published standard or a real linter rule.
//
// A finding that still resolves to "advisory" after this fallback (its class has no
// mapping, or it does not classify into any known class at all) is the CURATION GATE's
// job: `is_uncited_ai_finding` reports it as uncited, and `build_report_json` routes it
// to the informational/held-for-review bucket and EXCLUDES it from `curated_findings`
// entirely. It is never silently dropped (over-tell over under-tell) — it still surfaces
// as a `FindingRefJson` with an honest "needs review (uncited)" headline, just outside the
// curated action tiers.

/// Whether `finding` came from the AI tier (the model-inferred prose/deep-audit pass) as
/// opposed to the deterministic floor or a scan-time preview tool. Mirrors the signal
/// `ai_audit::finding_origin` already uses to distinguish AI output (an `AI-`-prefixed
/// invented rule id, or a calibration-set `confidence`) without reaching into that
/// module's private `Origin` enum — this file only needs the yes/no answer. Deliberately
/// gates the P3 citation fallback/exclusion to ONLY AI-tier findings: a deterministic
/// floor rule that happens to lack a corpus citation (a separate, pre-existing gap — e.g.
/// `SEC-NO-RAW-SQL-CONCAT-1`, which has no TOML entry at all) is untouched by this gate.
pub fn is_ai_tier(finding: &Finding) -> bool {
    finding.rule_id.starts_with("AI-") || finding.confidence.is_some()
}

/// The closed set of defect classes an otherwise-uncited AI-tier finding is checked
/// against before being allowed into the curated set (P3). Each variant names a corpus
/// rule ([`AiFindingClass::grounding_rule_id`]) whose `[[sources]]` are a real published
/// standard or linter — the class NEVER carries its own bespoke citation text, so there is
/// exactly one place (`crates/rules/principles/**`) an auditor edits to correct or extend
/// a citation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AiFindingClass {
    /// Row Level Security disabled, missing, or permissive on an exposed table (a
    /// cross-tenant read, RLS disabled on a payments table, an always-true policy, …).
    Rls,
    /// Stored or reflected cross-site scripting via a raw-HTML sink
    /// (`dangerouslySetInnerHTML`, `innerHTML`, `document.write`, `v-html`, …).
    Xss,
    /// A redirect target derived from user input with no allowlist check.
    OpenRedirect,
    /// A security token/nonce/session id generated from a general-purpose PRNG instead of
    /// a cryptographically secure source.
    WeakTokenRandomness,
    /// User-controlled data concatenated into a query/filter GRAMMAR (PostgREST
    /// `.or()`/`.filter()`, a NoSQL operator string, an LDAP filter) rather than passed as
    /// a bound argument.
    QueryGrammarInjection,
    /// A CORS policy pairing a wildcard or reflected `Origin` with
    /// `Access-Control-Allow-Credentials: true`.
    PermissiveCors,
}

impl AiFindingClass {
    /// The corpus rule id whose `sources` ground this class. Reused verbatim via
    /// `resolve_citation` — never re-derived here — so correcting or extending a citation
    /// is a one-file TOML change, not a second Rust-side copy to keep in sync.
    pub(crate) fn grounding_rule_id(self) -> &'static str {
        match self {
            AiFindingClass::Rls => "SUPABASE-RLS-ENABLED-1",
            AiFindingClass::Xss => "SEC-NO-UNSAFE-HTML-SINK-1",
            AiFindingClass::OpenRedirect => "SEC-NO-OPEN-REDIRECT-1",
            AiFindingClass::WeakTokenRandomness => "SEC-NO-WEAK-TOKEN-RANDOMNESS-1",
            AiFindingClass::QueryGrammarInjection => "SEC-NO-QUERY-GRAMMAR-INJECTION-1",
            AiFindingClass::PermissiveCors => "SEC-NO-PERMISSIVE-CORS-CREDENTIALS-1",
        }
    }
}

/// Classify an AI-tier finding into the P3 closed taxonomy, by RULE-ID TOKENS + the
/// existing semantic `category` field + DETAIL-TEXT keywords — never a fixture-specific
/// rule id or file/table name (the AI tier invents its own rule id per finding, so a
/// fixture-keyed match would only ever fire on one benchmark). General by construction:
/// every signal here is a defect-family vocabulary word (an OWASP/CWE term or the
/// framework API a real fix touches — `dangerouslySetInnerHTML`, `.or(`, `Origin`), not
/// any one repo's own identifiers. Returns `None` when nothing matches — the caller
/// (`is_uncited_ai_finding`) treats that as genuinely uncited, never guesses.
pub(crate) fn classify_ai_finding(
    rule_id: &str,
    category: Option<&str>,
    detail: &str,
) -> Option<AiFindingClass> {
    let id = rule_id.to_ascii_uppercase();
    let det = detail.to_ascii_lowercase();
    let has_id = |needle: &str| id.contains(needle);
    let has_det = |needle: &str| det.contains(needle);

    // RLS: disabled, missing, or overly permissive Row Level Security on an exposed table.
    if category == Some("rls-policy")
        || has_id("RLS")
        || has_id("ROW-LEVEL")
        || has_det("row level security")
        || has_det("row-level security")
    {
        return Some(AiFindingClass::Rls);
    }
    // Stored/reflected XSS via a raw-HTML sink.
    if has_id("XSS")
        || has_id("INNERHTML")
        || has_det("dangerouslysetinnerhtml")
        || has_det("innerhtml")
        || has_det("cross-site scripting")
        || has_det("cross site scripting")
        || has_det("stored xss")
        || has_det("reflected xss")
    {
        return Some(AiFindingClass::Xss);
    }
    // Open redirect: a redirect target with no allowlist check.
    if has_id("OPEN-REDIRECT")
        || has_det("open redirect")
        || has_det("unvalidated redirect")
        || (has_det("redirect")
            && (has_det("attacker")
                || has_det("untrusted")
                || has_det("arbitrary")
                || has_det("user-controlled")
                || has_det("unvalidated")))
    {
        return Some(AiFindingClass::OpenRedirect);
    }
    // Insecure randomness backing a security token/nonce/session id.
    if has_det("math.random")
        || has_det("random.random(")
        || has_det("insecure random")
        || has_det("weak random")
        || has_det("predictable token")
        || has_det("prng")
        || has_det("non-cryptographic")
        || ((has_id("RANDOM") || has_id("PRNG"))
            && (has_id("TOKEN") || has_id("SESSION") || has_id("NONCE") || has_id("SECRET")))
        || (has_det("random")
            && (has_det("token")
                || has_det("nonce")
                || has_det("session id")
                || has_det("password reset")
                || has_det("api key")
                || has_det("share link")))
    {
        return Some(AiFindingClass::WeakTokenRandomness);
    }
    // Query/filter-grammar injection: PostgREST .or()/.filter(), a NoSQL operator string,
    // an LDAP filter — built from interpolated user input rather than a bound argument.
    if has_id("FILTER-INJECT")
        || has_id("QUERY-GRAMMAR")
        || has_id("GRAMMAR-INJECT")
        || has_det(".or(")
        || has_det(".filter(")
        || has_det("query grammar")
        || has_det("filter grammar")
        || has_det("postgrest")
        || has_det("$where")
        || has_det("ldap filter")
    {
        return Some(AiFindingClass::QueryGrammarInjection);
    }
    // Permissive CORS: a wildcard/reflected origin combined with allowed credentials.
    let mentions_cors = has_id("CORS") || has_det("cors") || has_det("access-control-allow-origin");
    let mentions_credentials = has_id("CREDENTIAL")
        || has_det("credential")
        || has_det("allow-credentials")
        || has_det("cookie");
    if mentions_cors && mentions_credentials {
        return Some(AiFindingClass::PermissiveCors);
    }
    None
}

/// Resolve `finding`'s citation, applying the P3 class fallback when its OWN rule id has
/// no grounded corpus citation and no preview tool (`resolve_citation`'s "advisory"
/// branch) — but ONLY for an AI-tier finding (`is_ai_tier`); a deterministic finding's
/// advisory citation is a different, pre-existing gap this pass does not touch. Returns
/// the ORIGINAL citation unchanged in every other case (already grounded, already
/// preview-labeled, not AI-tier, or AI-tier but unclassifiable).
///
/// H5: a class-fallback citation is returned as kind `"related"` with its label prefixed by
/// [`RELATED_STANDARD_LABEL_PREFIX`]; its sources are the grounding rule's own, unchanged.
pub(crate) fn citation_for_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> CitationJson {
    let base = resolve_citation(&finding.rule_id, finding.preview_tool.as_deref(), corpus);
    if base.kind != "advisory" || !is_ai_tier(finding) {
        return base;
    }
    match classify_ai_finding(
        &finding.rule_id,
        finding.category.as_deref(),
        &finding.detail,
    ) {
        Some(class) => {
            let grounded = resolve_citation(class.grounding_rule_id(), None, corpus);
            if grounded.kind == "grounded" {
                CitationJson {
                    kind: "related".to_string(),
                    label: format!("{RELATED_STANDARD_LABEL_PREFIX}{}", grounded.label),
                    sources: grounded.sources,
                }
            } else {
                base
            }
        }
        None => base,
    }
}

/// Human-readable title for `finding`'s rule (C4-P2). Prefers, in order:
/// 1. the finding's OWN rule id's corpus title — unchanged from every pre-existing call site
///    (a deterministic/preview finding always has a corpus entry, so this is the only branch
///    that ever fires for one);
/// 2. for an AI-tier finding whose own invented rule id has no corpus entry, the GROUNDING
///    rule's corpus title — the same rule [`citation_for_finding`] already borrows a citation
///    from for this finding's class, so the header and the citation underneath it describe the
///    SAME standard rather than a human-readable citation sitting under a header that is
///    nothing but the model's own invented id repeated;
/// 3. the bare rule id — truly unclassifiable, last resort, never fabricated (identical to the
///    pre-existing behavior for this case).
pub(crate) fn title_for_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(&finding.rule_id)) {
        return rule.title.clone();
    }
    if is_ai_tier(finding) {
        if let Some(class) = classify_ai_finding(
            &finding.rule_id,
            finding.category.as_deref(),
            &finding.detail,
        ) {
            if let Some(rule) = corpus.and_then(|c| c.get_by_id(class.grounding_rule_id())) {
                return rule.title.clone();
            }
        }
    }
    finding.rule_id.clone()
}

/// C4-P4 (residual defect 2): human-readable TITLES for a finding's `also_matches` list (other
/// rule ids absorbed into this one during P1 clustering) — NEVER the raw rule ids themselves.
/// Client-facing prose (the PDF curated-site "Also violates" line, the xlsx/`findings.json`
/// "Also matches" column) must never surface an internal rule-id token, which a reader has no
/// way to decode. An id with no resolvable corpus title is DROPPED from the list entirely
/// rather than falling back to the bare id — unlike [`title_for_finding`]'s last-resort bare-id
/// fallback (fine for a finding's OWN primary title, since that's the only identifier left to
/// show at all) applying that same fallback here would just reintroduce the raw-id leak this
/// function exists to close. Shared by both call sites so the PDF and the xlsx/JSON sibling can
/// never render two different label sets for the same underlying ids.
pub(crate) fn also_matches_titles(
    ids: &[String],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<String> {
    ids.iter()
        .filter_map(|id| {
            corpus
                .and_then(|c| c.get_by_id(id))
                .map(|rule| rule.title.clone())
        })
        .collect()
}

/// The P3 curation gate: true when `finding` is AI-tier AND its citation is still
/// "advisory" after the class-fallback attempt above — i.e. it cannot be honestly
/// presented as grounded. `build_report_json` excludes such a finding from
/// `curated_findings` entirely and routes it to the informational/held-for-review bucket
/// with an explicit "needs review (uncited)" headline, REGARDLESS of severity — unlike
/// `is_informational`'s other signals, this one is a report-integrity gate, not a
/// triage-confidence signal: a critical, uncited finding is PRECISELY the case this gate
/// exists to catch, so it is never dropped, never silently demoted into an ordinary
/// severity-capped appendix row, and (C5-7) never squashed into `informational` at a
/// severity that invariant was never meant to carry — see [`effective_bucket`]'s doc comment
/// for the held-vs-informational split this feeds into (held at high/critical, informational
/// at medium/low/info, both still fully visible and both still excluded from
/// `curated_findings`).
pub(crate) fn is_uncited_ai_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> bool {
    is_ai_tier(finding) && citation_for_finding(finding, corpus).kind == "advisory"
}

/// Join `rule_id` against the loaded corpus to recover its DEFAULT option's AUTHORED
/// `remediation` text, then instantiate that text's placeholder tokens (`<table>`,
/// `<function-name>`, `<bucket>`, `<path>`, …) from `finding`'s own `path`/`captures`. This is a
/// serialization-time join exactly like [`resolve_citation`] for the RULE-level lookup, plus a
/// per-FINDING instantiation step — the authored text is a property of the rule, but naming the
/// specific table/function/bucket in THIS codebase is a property of the finding.
///
/// # `remediation`, never `directive` (2026-09-13 product-review Fix 1)
/// `remediation` is an authored field with one job: 2-4 imperative sentences telling the
/// CLIENT what to change, where, and how to verify the fix. This function binds ONLY to it and
/// must NEVER fall back to `directive` or any other rule field when it is absent — fail-safe
/// here means silently omitting the Fix block. See
/// `docs/design/2026-09-13_audit-deliverable-review-fixes.md` Fix 1.
///
/// **Correction, 2026-10-20.** This comment used to justify that rule by calling `directive`
/// "the rule's DETECTION/enforcement recipe — internal, agent-facing instructions for how
/// Camerata itself finds this defect", which "leaks internals and confuses a paying client".
/// The BEHAVIOUR is right and unchanged; the characterisation was wrong, and
/// `docs/RULE_AUTHORING.md`'s 2026-10-19 correction identifies it as the misreading that let
/// detector voice into the corpus in the first place.
///
/// `directive` is not internal. `arm.rs::render_rule` emits it VERBATIM into the client's own
/// `AGENTS.md`/`CONVENTIONS.md` as a convention their project follows — which is the product,
/// not a leak. A well-written directive is a behavioural commitment an engineer or agent in
/// that repo can follow, and it is perfectly fine in front of a client. What is not fine is a
/// directive written as a scanner recipe, and that text is wrong for the scanning MODEL too,
/// which gets told to behave like a regex engine instead of being told what the code must do.
///
/// So the reason this function binds only to `remediation` is narrower than the old comment
/// claimed, and still sound: a directive states what the code must BE, while a Fix line must
/// say what to CHANGE and how to verify it. Those are different sentences, and substituting
/// one for the other produces a Fix block that restates the problem.
///
/// Returns `None` (never a fabricated sentence, never `directive`) when: the corpus is absent,
/// the rule id has no corpus entry, the rule has no options at all, the rule has no adopted
/// DEFAULT option, or the resolved option's `remediation` field is itself absent/blank in the
/// TOML (not yet authored — the common case until the corpus-wide authoring pass in Phase 2a
/// lands). Surfaced as the PDF's "Fix:" line (`CuratedSiteJson::fix`) and the workbook's
/// "Recommended Fix" column / `findings.json`'s `fix` field (`FindingRow::fix`) — same join, all
/// three, so an absent remediation reads as an honestly OMITTED Fix block everywhere rather than
/// one artifact inventing text the others don't have.
/// Resolve for the option this finding was ACTUALLY evaluated under, so the report's Fix
/// text matches what was judged, never a stale default (2026-09-22 audit-integrated
/// alternatives, extending the 2026-09-13 Fix-1 mechanism below). Resolution order, via
/// [`camerata_rules::Rule::resolved_option`]:
/// 1. `finding.evaluated_option_id` — the option the audit (or a targeted
///    `rescan-alternatives` call) actually judged this specific finding against, when the
///    rule is multi-option and semantic;
/// 2. `chosen_option` — the project's persisted `RuleSelection.chosen_option` for this rule,
///    passed in by the caller (`ReportOptions::chosen_options`), when the finding predates
///    this field or belongs to a rule the AI audit never tagged (e.g. a legacy persisted
///    finding);
/// 3. the corpus rule's own `default_option` — `resolved_option`'s own fallback, unchanged
///    from before this feature.
pub(crate) fn resolve_fix(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
    chosen_option: Option<&str>,
) -> Option<String> {
    resolve_fix_inner(rule_id, corpus, finding, chosen_option, false)
}

/// R1-3: [`resolve_fix`] for a consistency DEVIATION row. When the option the row was judged
/// under has no authored remediation of its own (an alternative such as `parse-in-handler` in
/// `arch-typed-path-params-1.toml`), the rule's DEFAULT option's remediation ships instead, as
/// the rule's standard fix, so a deviation row never goes out with an empty fix while the rule
/// has one. [`resolve_fix`] itself stays strict (an option with no remediation omits the Fix
/// block): for a defect row the default option's advice may not fit the option in force.
pub(crate) fn resolve_deviation_fix(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
    chosen_option: Option<&str>,
) -> Option<String> {
    resolve_fix_inner(rule_id, corpus, finding, chosen_option, true)
}

/// Kind-aware [`resolve_fix`] used by every export surface that prints a row's fix (the PDF
/// data, `findings.json`, the datasets and the workbook, and the held-for-review routing): a
/// consistency deviation row resolves through [`resolve_deviation_fix`], every other row stays
/// strict, so one row never has a fix in one artifact and none in another.
pub(crate) fn resolve_fix_for_row(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
    chosen_option: Option<&str>,
) -> Option<String> {
    if matches!(
        finding.kind,
        Some(camerata_api_types::audit::FindingKind::Deviation(_))
    ) {
        resolve_deviation_fix(rule_id, corpus, finding, chosen_option)
    } else {
        resolve_fix(rule_id, corpus, finding, chosen_option)
    }
}

pub(crate) fn resolve_fix_inner(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
    chosen_option: Option<&str>,
    fall_back_to_rule_default: bool,
) -> Option<String> {
    let rule = corpus_rule_for(rule_id, corpus)?;
    let evaluated = finding.evaluated_option_id.as_deref().or(chosen_option);
    let authored = |o: &camerata_rules::RuleOption| {
        o.remediation
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string)
    };
    let remediation = rule
        .resolved_option(evaluated)
        .and_then(authored)
        .or_else(|| {
            fall_back_to_rule_default
                .then(|| rule.resolved_option(None).and_then(authored))
                .flatten()
        })?;
    let remediation = remediation.as_str();
    // The authored text is a TOML multi-line string: join its hard line wraps here, once, so no
    // artifact ever sees a mid-sentence newline (see `fix_text`).
    Some(fix_text::prepare(&instantiate_remediation(
        remediation,
        &finding.path,
        &finding.captures,
    )))
}

/// C5-8 (R7 amended): whether `finding` has ANY fix text available to show a client, if it
/// lands in an action bucket — either a this-codebase specific fix
/// (`ai_audit::generate_fix_specifics`'s `Finding::fix_specific`) or the rule's own authored
/// remediation ([`resolve_fix`], the exact join `CuratedSiteJson::fix` itself performs). A
/// pipeline failure on the FIRST (fix-generation gave up or never ran) is routine and never
/// gates anything by itself — see the C3-1b module comment above — because the SECOND normally
/// still ships a usable fix.
///
/// Deliberately does NOT, by itself, treat "the rule id has no corpus entry at all" as missing
/// a fix — see [`is_action_row_missing_a_fix`], the actual bucket-routing gate, for why that
/// case is scoped out separately rather than folded in here.
pub(crate) fn has_any_fix(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_option: Option<&str>,
) -> bool {
    finding
        .fix_specific
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || resolve_fix_for_row(&finding.rule_id, corpus, finding, chosen_option).is_some()
}

// ── P4 (2026-09-29): floor findings get finding-level treatment ────────────────────
//
// A deterministic/floor finding's `detail` is set at scan time
// (`onboard::audit::title_for`) from the GATE's own rule description — internal, agent-facing
// enforcement prose ("Deny writing a file whose path marks it as secret-bearing…"), never
// authored for a client to read. Left alone, that text flows straight into both the headline
// (`defect_headline` takes its first sentence) and the detail of every curated site for a
// floor rule. [`resolve_floor_finding_text`] is the render-time escape hatch, mirroring
// [`resolve_fix`] exactly: an AUTHORED template lives on the rule's resolved option in the
// corpus (`RuleOption::finding_headline` / `finding_detail`), instantiated from the SAME
// `<path>`/`<file>`/capture placeholders `resolve_fix` already substitutes, so a rule author
// writes ONE specific, plain-language pair per rule and every finding of that rule gets a
// repo-specific rendering for free. `is_ai_tier` findings never reach this function in
// practice (they invent their own rule id, which the corpus never resolves) but nothing here
// assumes that — it degrades to `None` for any rule id the corpus doesn't know, same as
// `resolve_fix`.

/// Resolve the authored, client-facing (headline, detail) pair for a DETERMINISTIC FLOOR
/// finding (P4), instantiating `<path>`/`<file>`/capture placeholders exactly as
/// [`resolve_fix`] does for `remediation`. Returns `None` — the caller then falls back to the
/// pre-P4 `defect_headline(&finding.detail, …)` / `finding.detail.clone()` behavior — when: the
/// corpus is absent, the rule id has no corpus entry, the rule has no resolvable option, or
/// either `finding_headline` or `finding_detail` is absent/blank on that option (authored as a
/// PAIR; never render one half authored and the other the gate's raw text). This is the ONLY
/// path by which a floor finding's client-facing headline/detail can differ from its raw
/// `Finding::detail` — see the module-level comment above for why that indirection exists.
pub(crate) fn resolve_floor_finding_text(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
    chosen_option: Option<&str>,
) -> Option<(String, String)> {
    let rule = corpus.and_then(|c| c.get_by_id(rule_id))?;
    let evaluated = finding.evaluated_option_id.as_deref().or(chosen_option);
    let option = rule.resolved_option(evaluated)?;
    let headline_tpl = option.finding_headline.as_deref()?.trim();
    let detail_tpl = option.finding_detail.as_deref()?.trim();
    if headline_tpl.is_empty() || detail_tpl.is_empty() {
        return None;
    }
    let headline = instantiate_remediation(headline_tpl, &finding.path, &finding.captures);
    let detail = instantiate_remediation(detail_tpl, &finding.path, &finding.captures);
    Some((headline, detail))
}

/// Client-facing (headline, detail) for ONE finding, preferring the P4 authored floor
/// template ([`resolve_floor_finding_text`]) and falling back to the pre-P4 derivation
/// (`defect_headline` over `finding.detail`, `finding.detail` verbatim) when no authored
/// template is available for this rule/option — an AI-tier finding (whose own prose already
/// leads with a plain-language sentence by house convention) or a floor rule not yet authored.
/// Centralizing this join means the matrix headline and the curated site's headline+detail can
/// never drift onto two different derivations of the same finding.
pub(crate) fn client_headline_and_detail(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
    fallback_title: &str,
    chosen_option: Option<&str>,
) -> (String, String) {
    let (headline, detail) =
        match resolve_floor_finding_text(&finding.rule_id, corpus, finding, chosen_option) {
            Some((headline, detail)) => (headline, detail),
            None => (
                defect_headline(&finding.detail, fallback_title),
                finding.detail.clone(),
            ),
        };
    // A headline or detail the reviewer edited is theirs: a corpus template must not rewrite it.
    let reviewer_text = |field| {
        overlay::reviewer_text(finding, field)
            .filter(|_| overlay::reviewer_edited_field(finding, field))
    };
    // Whoever wrote it, a headline and a detail reach the client without a dash, a multiplication
    // sign or a hedge label in front (`Borderline:`), and a whole-file place reads "whole file".
    let (headline, detail) = (
        reviewer_text(camerata_api_types::edits::EditField::Headline)
            .map_or(headline, str::to_string),
        reviewer_text(camerata_api_types::edits::EditField::Detail).map_or(detail, str::to_string),
    );
    (
        crate::client_text::without_hedge_label(&crate::report_export::client_prose(&headline))
            .into_owned(),
        crate::report_export::client_prose(&detail),
    )
}

/// The readable, never-internal-sounding generic noun phrase substituted for a placeholder
/// token when the finding carries no captured value for it. Keyed by the token's bare name
/// (angle brackets stripped) exactly as it appears in authored `remediation` (and, before it,
/// the corpus rules' own `directive`) text — extend this map when a new rule introduces a new
/// domain token, so its remediation can still render something readable before a detector is
/// wired to capture the real object. `<path>`/`<file>` are handled separately in
/// [`instantiate_remediation`] (filled from the finding's own `path`, which is always present),
/// so they are not listed here; the catch-all arm below is still a safe fallback for them too if
/// `path` is ever empty.
/// True when `token` (already stripped of any leading `a:` prefix) has the SHAPE every real
/// placeholder in this corpus uses: lowercase ASCII letters, digits, and hyphens only (`path`,
/// `function-name`, `secret-kind`, …). C4-R3: authored `remediation`/`finding_*` prose
/// occasionally needs to show a literal angle-bracketed EXAMPLE inline — e.g. an XSS rule's
/// "verify a payload like <img src=x onerror=alert(1)> renders as inert text" — and
/// [`instantiate_remediation`] used to treat every `<...>` span as a token to fill, so that
/// literal example's own `<img ...>` markup got swallowed and replaced by the generic filler,
/// rendering the nonsense sentence "a payload like the affected resource renders as inert
/// text." A real token is never spelled with spaces, `=`, or uppercase letters; this check lets
/// [`instantiate_remediation`] tell the two apart and leave a non-token span exactly as the
/// rule author wrote it.
pub(crate) fn is_placeholder_token_shape(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn generic_placeholder_filler(token: &str) -> &'static str {
    match token {
        "table" => "the affected table",
        "function-name" | "function" => "the affected function",
        "bucket" => "the affected bucket",
        "view" => "the affected view",
        "matview" => "the affected materialized view",
        "schema" => "the affected schema",
        "policy" => "the affected policy",
        "path" | "file" => "the affected file",
        // P4: the secret-shaped floor rules' context-fact tokens (`onboard::audit::
        // enrich_secret_context` / `onboard::attach_secret_history_capture`). The first two
        // are always populated once the rule fires (the enrichment is unconditional and
        // pure); `history-status` is best-effort (needs a real local git checkout), so this
        // fallback is its realistic path, not just defensive dead code.
        // Bare noun (no leading article) — see `onboard::audit::classify_secret_kind`'s doc
        // comment (W4, 2026-09-30): this fallback shares that contract so it can never double
        // an article a template already supplies, and can be safely used with `<a:secret-kind>`
        // where the template wants one computed dynamically instead.
        "secret-kind" => "hardcoded credential",
        "gitignore-status" => "not confirmed against this repository's `.gitignore`",
        "history-status" => "not checked against this repository's commit history",
        _ => "the affected resource",
    }
}

/// Acronym-initial letters whose NAME (when read letter-by-letter, as an acronym is) begins
/// with a vowel SOUND even though the letter itself is a consonant: "F" is "ef", "H" is
/// "aitch", "L" is "el", "M" is "em", "N" is "en", "R" is "ar", "S" is "es", "X" is "ex".
/// English indefinite-article choice follows the SOUND, not the letter — "an SSH key", "an
/// FAQ", "an HTML page", "an MRI" are all correct despite S/F/H/M being consonants. Used by
/// [`indefinite_article`].
pub(crate) const VOWEL_SOUND_ACRONYM_INITIALS: &[char] = &['F', 'H', 'L', 'M', 'N', 'R', 'S', 'X'];

/// Choose `"a"` or `"an"` for the noun phrase `phrase`, based on its first word. Handles the
/// two cases the corpus's authored templates actually need: an ordinary word (vowel-LETTER
/// check) and an ALL-CAPS acronym read letter-by-letter (vowel-SOUND check via
/// [`VOWEL_SOUND_ACRONYM_INITIALS`]) — e.g. `"SSH private key"` needs `"an"`, not `"a"`,
/// because "S" is pronounced "ess". Deliberately scoped to this plain technical vocabulary —
/// no dictionary of irregular exceptions (`"hour"`, `"European"`, …) that this corpus's
/// substituted values never produce. See [`instantiate_remediation`]'s `<a:token>` handling,
/// the caller this exists for.
pub(crate) fn indefinite_article(phrase: &str) -> &'static str {
    let first_word = phrase.split_whitespace().next().unwrap_or("");
    let Some(first_char) = first_word.chars().next() else {
        return "a";
    };
    let is_acronym = first_word.len() >= 2 && first_word.chars().all(|c| c.is_ascii_uppercase());
    let starts_with_vowel_letter =
        matches!(first_char.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u');
    // A vowel-LETTER initial sounds like a vowel whether or not the word is an acronym read
    // letter-by-letter ("AWS" -> "ay") — that case is already covered by the plain check. The
    // acronym exception set only needs to add the CONSONANT letters whose own name starts with
    // a vowel sound ("S" -> "ess").
    let vowel_sound = starts_with_vowel_letter
        || (is_acronym && VOWEL_SOUND_ACRONYM_INITIALS.contains(&first_char));
    if vowel_sound {
        "an"
    } else {
        "a"
    }
}

/// Substitute every `<token>` placeholder in authored `remediation` text with a concrete value,
/// so the same authored sentence names the actual object in THIS codebase. Resolution order per
/// token: (1) `<path>`/`<file>` always resolve to `path` (the finding's own path) when it is
/// non-empty; (2) any other token resolves to `captures.get(token)` when the detector populated
/// one; (3) otherwise a readable generic from [`generic_placeholder_filler`]. This function is
/// the SOLE gate between authored text and the client, so it is deliberately exhaustive: EVERY
/// well-formed `<...>` span found in `remediation` is replaced by construction — there is no
/// code path that can leave a raw `<token>` in the returned string. A malformed span (a bare `<`
/// with no matching `>` anywhere after it) is passed through unmodified rather than guessed at,
/// since that is prose, not a token — this can only happen if a rule author literally types an
/// unmatched `<` in remediation copy, not from anything a detector or the finding data supplies.
///
/// # `<a:token>` — a dynamically-chosen indefinite article (W4, 2026-09-30)
/// A token spelled `<a:token>` (e.g. `<a:secret-kind>`) resolves `token` exactly as above, then
/// prepends `"a "`/`"an "` (via [`indefinite_article`]) to the resolved value. Every substituted
/// value in this corpus is now a bare noun phrase with NO leading article of its own (see
/// `onboard::audit::classify_secret_kind`'s doc comment) — a template either supplies its own
/// fixed article ahead of an adjective (`"a live <secret-kind>"`, correct regardless of the
/// noun's own sound, since the article agrees with "live") or, when the placeholder opens the
/// clause with nothing of its own in front of it, requests `<a:token>` so the RESOLVED value's
/// own initial sound picks the right article. Before this existed, a template could only
/// hardcode a literal article, which silently DOUBLED whenever the substituted value already
/// carried one of its own (`"a live a vendor credential token"`) — the exact W4 bug class this
/// mechanism removes structurally, for this token and any future one.
pub(crate) fn instantiate_remediation(
    remediation: &str,
    path: &str,
    captures: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut out = String::with_capacity(remediation.len());
    let mut rest = remediation;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];
        let Some(end) = after_open.find('>') else {
            // No closing bracket anywhere ahead — not a well-formed token. Emit the remainder
            // verbatim rather than treating a stray `<` as something to defend against.
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let raw_token = after_open[..end].trim();
        let (token, want_article) = match raw_token.strip_prefix("a:") {
            Some(bare) => (bare.trim(), true),
            None => (raw_token, false),
        };
        if !is_placeholder_token_shape(token) {
            // C4-R3: not a real placeholder — emit the original bracketed span verbatim. Running
            // a literal example (HTML markup, a generic-type snippet, …) through the generic
            // filler would replace the author's own words with nonsense; a malformed span gets
            // the identical verbatim treatment just below in the no-closing-bracket branch.
            out.push_str(&rest[start..start + 1 + end + 1]);
            rest = &after_open[end + 1..];
            continue;
        }
        let filled = if matches!(token, "path" | "file") && !path.trim().is_empty() {
            path.to_string()
        } else {
            captures
                .get(token)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| generic_placeholder_filler(token).to_string())
        };
        let filled = if want_article {
            format!("{} {filled}", indefinite_article(&filled))
        } else {
            filled
        };
        out.push_str(&filled);
        rest = &after_open[end + 1..];
    }
    out.push_str(rest);
    out
}

// ── Export reproducibility: one clock reading, taken from the scan ───────────────
