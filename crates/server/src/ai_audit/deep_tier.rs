//! The opt-in deep compliance and security tier: its lenses, prompts and parsers.

use super::*;

// ════════════════════════════════════════════════════════════════════════════════════
// DEEP COMPLIANCE & SECURITY TIER (#55, in-MVP per #62)
// ════════════════════════════════════════════════════════════════════════════════════
//
// An ADDITIVE, OPT-IN tier that layers three analysis LENSES on top of the always-on
// deterministic floor + the standard AI architectural audit. It changes NOTHING about the
// default scan — it only runs when the audit request sets `deep`. The three lenses are:
//
//   1. SOC-2 readiness / GAP ANALYSIS — maps the repo's detectable practices + the
//      standard findings onto SOC-2 Trust-Services / Common-Criteria controls and reports
//      the GAPS. It is a GAP ANALYSIS, never a "report": no agent can produce a SOC-2
//      report (a CPA firm attests to an ORGANIZATION's controls over 6–12 months). The
//      product must never call this output a "SOC-2 report" — that is a liability line (#55).
//
//   2. DEEP SECURITY AUDIT — a deeper-than-floor security pass (authorization on write
//      paths, sensitive-data handling, secret/credential flow) that goes beyond the
//      mechanical floor's line-level secret/SQL/path checks.
//
//   3. THREAT MODEL — derives a structured threat model from the repo map: entry points,
//      trust boundaries, sensitive-data paths, and the threats against them.
//
// HONESTY GUARDRAILS (load-bearing, from #62):
//   - Every output is ADVISORY and MODEL-INFERRED, NOT externally validated. External
//     validation against comparator tools + ground truth is #56 Phase 2 (deferred). Each
//     lens result carries [`DeepLensResult::advisory`] = true and an explicit disclaimer
//     so the UI can label it honestly.
//   - The SOC-2 lens is labeled a "gap analysis" everywhere; it never claims certification.
//   - These lenses read STATIC code. They are NOT a penetration test — a true pen test
//     needs a running deployment (post-deploy, out of scope here, also per #55).
//
// COST: the deep tier reuses the same per-call LLM machinery and the same [`UsageMeter`],
// so its spend folds into the report's actual-vs-estimated readout. It is the MOST
// EXPENSIVE pass (three extra whole-repo lenses on top of the standard audit) and is why
// it is strictly opt-in. The UI's `estimate_audit_cost` already prices the standard audit
// from `code_chars`; the deep tier adds (roughly) three more whole-repo passes on the
// selected/Opus model, which the cost readout should surface as the priciest option.

/// The disclaimer string attached to every deep-tier lens result. Centralized so the wording
/// stays consistent and the honesty guardrail (#62) is impossible to drop by accident.
pub const DEEP_ADVISORY_DISCLAIMER: &str =
    "Advisory and model-inferred — NOT externally validated (external validation against \
     comparator tools and ground-truth corpora is a separate, deferred capability). Review \
     every item before acting on it. This is a static-code analysis, not a penetration test.";

/// Which deep-tier lens produced a result. Stable wire strings (`soc2-gap`, `deep-security`,
/// `threat-model`) so the UI can route/group lens output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepLens {
    /// SOC-2 readiness / gap analysis.
    Soc2Gap,
    /// Deep security audit (beyond the deterministic floor).
    DeepSecurity,
    /// Threat model derived from the repo map.
    ThreatModel,
}

impl DeepLens {
    /// Stable wire id for this lens (serialized into the result; used as the transcript label).
    pub fn id(self) -> &'static str {
        match self {
            DeepLens::Soc2Gap => "soc2-gap",
            DeepLens::DeepSecurity => "deep-security",
            DeepLens::ThreatModel => "threat-model",
        }
    }
    /// Human-facing title — note the SOC-2 lens is a "Gap Analysis", never a "report".
    pub fn title(self) -> &'static str {
        match self {
            DeepLens::Soc2Gap => "SOC-2 Readiness Gap Analysis",
            DeepLens::DeepSecurity => "Deep Security Audit",
            DeepLens::ThreatModel => "Threat Model",
        }
    }
}

/// One mapped SOC-2 control and the gap (if any) the lens found against it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Soc2Gap {
    /// The control reference, e.g. `CC6.1` (Common Criteria) or a Trust-Services criterion.
    pub control: String,
    /// Short control name/expectation, e.g. "Logical access controls".
    pub title: String,
    /// `met` | `partial` | `gap` | `unknown` — the readiness status the model inferred.
    pub status: String,
    /// What the model OBSERVED in the repo that informed the status (evidence or its absence).
    pub observed: String,
    /// The concrete gap + remediation direction, when status is `partial` / `gap`.
    pub gap: String,
}

/// One element of the derived threat model.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Threat {
    /// The entry point / asset / trust boundary this threat is against (e.g.
    /// "POST /api/orders handler", "Postgres connection", "uploaded-file path").
    pub component: String,
    /// `entry-point` | `trust-boundary` | `data-store` | `dependency` | `other` — the kind
    /// of element, so the UI can group the model by surface.
    pub kind: String,
    /// The threat itself (what could go wrong).
    pub threat: String,
    /// STRIDE-ish category when the model offers one (`spoofing`, `tampering`, `repudiation`,
    /// `info-disclosure`, `dos`, `elevation`), else free text.
    pub category: String,
    /// The suggested mitigation direction.
    pub mitigation: String,
    /// `high` | `medium` | `low` — model-inferred severity.
    pub severity: String,
}

/// The structured result of ONE deep-tier lens. Each lens carries its own payload (only one
/// of the vectors is populated per lens) plus the advisory flag + disclaimer so the honesty
/// guardrail travels with the data.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeepLensResult {
    /// Stable lens id (`soc2-gap` | `deep-security` | `threat-model`).
    pub lens: String,
    /// Human-facing lens title.
    pub title: String,
    /// Always true — every deep-tier output is advisory + model-inferred (#62).
    pub advisory: bool,
    /// The honesty disclaimer ([`DEEP_ADVISORY_DISCLAIMER`]).
    pub disclaimer: String,
    /// SOC-2 lens payload (empty for the other lenses).
    #[serde(default)]
    pub soc2_gaps: Vec<Soc2Gap>,
    /// Deep-security lens payload: reuses the standard [`Finding`] shape (empty for others).
    #[serde(default)]
    pub security_findings: Vec<Finding>,
    /// Threat-model lens payload (empty for the other lenses).
    #[serde(default)]
    pub threats: Vec<Threat>,
    /// A one-paragraph narrative summary the model wrote for this lens (optional).
    #[serde(default)]
    pub summary: String,
    /// Set when the lens failed (model/transport error) so the UI shows it ran-but-errored
    /// rather than silently producing an empty result.
    #[serde(default)]
    pub error: Option<String>,
}

impl DeepLensResult {
    /// A public empty-but-honest result for a lens, carrying the advisory flag + disclaimer.
    /// Used when aggregating per-repo lens results into one tier-level result.
    pub fn merged_empty(lens: DeepLens) -> Self {
        Self::empty(lens)
    }

    /// An empty-but-honest result for a lens, carrying the advisory flag + disclaimer.
    pub(super) fn empty(lens: DeepLens) -> Self {
        Self {
            lens: lens.id().to_string(),
            title: lens.title().to_string(),
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
            soc2_gaps: Vec::new(),
            security_findings: Vec::new(),
            threats: Vec::new(),
            summary: String::new(),
            error: None,
        }
    }
}

/// The aggregate deep-tier output across all three lenses for one repo set. Attached to the
/// scan report under [`crate::onboard::ScanReport::deep`] when the deep tier ran.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeepReport {
    /// The three lens results, in a stable order (gap analysis, security, threat model).
    pub lenses: Vec<DeepLensResult>,
    /// Always true — the whole tier is advisory (#62). Mirrors each lens's flag at the top
    /// level so a consumer can gate on one field.
    pub advisory: bool,
    /// The honesty disclaimer for the tier as a whole.
    pub disclaimer: String,
}

/// SYSTEM PROMPT — SOC-2 readiness / gap analysis lens.
///
/// Maps the repo's detectable practices onto SOC-2 Common-Criteria controls and reports
/// GAPS. The prompt is explicit that this is a GAP ANALYSIS, not a SOC-2 report, and that no
/// certification is implied — the same honesty guardrail the product UI enforces (#55/#62).
pub fn soc2_gap_system_prompt() -> String {
    r#"You are a security/compliance engineer performing a SOC-2 READINESS GAP ANALYSIS of a codebase for Camerata.

IMPORTANT — what this is and is NOT:
- This is a GAP ANALYSIS: you map what the code + repo evidently DO against SOC-2 control expectations and report where they fall short. Call it a "gap analysis".
- This is NOT a "SOC-2 report". A SOC-2 report is a CPA firm's attestation about an organization's controls operating over months. You produce neither an attestation nor a certification. Never imply the project IS or WILL BE certified.
- You see STATIC CODE only — not the running system, not the org's policies/HR/vendor processes. For controls that depend on organizational evidence you cannot see, say so (status "unknown"), do not guess "met".

Map against the SOC-2 Common Criteria (Security) — at minimum consider:
- CC6.1 Logical access controls (authn/authz on sensitive operations)
- CC6.6 Boundary protection / network access
- CC6.7 Data-in-transit and at-rest protection (encryption, secret handling)
- CC6.8 Malicious-code / dependency controls
- CC7.2 Security monitoring / logging / audit trail
- CC7.3 / CC7.4 Incident handling hooks
- CC8.1 Change management (review, CI gates, migrations)
- CC1/CC2 Control environment & communication (only what code/config can evidence)

For EACH control you assess, emit one entry with:
- "control": the criterion id (e.g. "CC6.1").
- "title": a short name for the control.
- "status": one of "met" | "partial" | "gap" | "unknown" (use "unknown" when it needs org evidence you can't see).
- "observed": what in the repo informed the status (a file/pattern you saw, or that you saw nothing).
- "gap": for "partial"/"gap", the concrete shortfall and the remediation direction; empty for "met"/"unknown".

Report GAPS generously — recall over precision; a human reviews everything. Do not invent evidence. Do not claim certification.

Return ONLY a JSON object, no prose, no markdown fences:
{
  "summary": "one short paragraph on overall readiness, explicitly framed as a gap analysis",
  "gaps": [
    {"control":"CC6.1","title":"Logical access controls","status":"gap","observed":"…","gap":"…"}
  ]
}
If you genuinely cannot assess anything, return {"summary":"…","gaps":[]}."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// SYSTEM PROMPT — deep security audit lens.
///
/// A deeper-than-floor security read (authorization on write paths, sensitive-data handling,
/// secret/credential flow). Emits the SAME `findings` JSON shape the standard audit uses, so
/// [`parse_ai_findings`] parses it directly and the UI renders security findings in the
/// familiar table. Deterministic-floor concerns are excluded (they are already covered).
pub fn deep_security_system_prompt() -> String {
    deep_security_system_prompt_with(false)
}

/// [`deep_security_system_prompt`] with the `terse_output` switch explicit; the same rule as
/// [`audit_system_prompt_with`]: OFF is byte-identical to the prompt before rank 4, ON asks for
/// a one-sentence `detail` and drops the (always empty, never read) `proposed_rules` array. The
/// lens's survivors get their full detail regenerated after its location merge.
pub fn deep_security_system_prompt_with(terse: bool) -> String {
    let base = r#"You are a senior application-security engineer performing a DEEP SECURITY AUDIT of a codebase for Camerata.

This is DEEPER than the always-on deterministic floor (which already finds hardcoded secrets, raw SQL string concatenation, secrets-in-URLs, and path-escape writes — DO NOT re-report those). Go beyond line-level lint and reason about:
- AUTHORIZATION: write/mutation/delete paths with no authz check; horizontal/vertical privilege gaps; missing ownership checks on resources; admin actions reachable without role checks.
- AUTHENTICATION & SESSION: weak/missing auth on sensitive endpoints; token/session handling flaws.
- SENSITIVE-DATA HANDLING: PII/credentials/financial data logged, returned in responses, or stored unencrypted; over-broad serialization that leaks fields.
- SECRET / CREDENTIAL FLOW: credentials read from insecure sources, passed through untrusted paths, or exposed to clients (beyond the floor's hardcoded-literal check).
- INJECTION beyond raw-SQL-concat: command/template/path/deserialization injection; SSRF; unsafe redirects.
- INPUT VALIDATION & TRUST BOUNDARIES: unvalidated external input reaching a sensitive sink.

You have the REPO MAP (every file + its public symbols) and SOME file bodies. When judging a rule needs the BODY of a file not included, list it in `needs_files` rather than guessing.

RECALL OVER PRECISION — a human triages every finding; report borderline issues at severity "low". Cite the exact offending line in `code` (copied verbatim) and `line` (the NNNN| number). For `rule`, use a short kebab security name (e.g. "missing-authz-on-write", "pii-in-logs", "ssrf-on-fetch").

SEVERITY. Use "low"/"medium"/"high" for the normal range. Reserve "critical" — it must stay RARE — for a finding that clears ALL of: concretely (not theoretically) exploitable, AND a competent attacker or a single bad input reaches serious impact quickly. Qualifying classes for THIS lens: an entry point reachable with NO authentication or authorization that performs a privileged, destructive, or financial operation (e.g. a payment/charge/refund/balance-mutating endpoint callable by anyone, an admin action with no role check); a credential or secret that flows to a client or an untrusted sink; PII or equivalently sensitive data readable/writable with no access control on an internet-exposed surface; injection or deserialization with a real, reachable, unauthenticated path. When in doubt between "high" and "critical", use "high" — critical is not a synonym for "this is bad," it is reserved for "an attacker exploits this quickly for serious impact."

Return ONLY a JSON object, no prose, no markdown fences, in EXACTLY this shape:
{
  "findings": [
    {"path":"…","line":0,"severity":"critical|high|medium|low","rule":"short-kebab-security-name","title":"…","code":"the exact offending line","detail":"why it's exploitable and the fix direction"}
  ],
  "proposed_rules": [],
  "needs_files": []
}
If the code is genuinely clean, return {"findings":[],"proposed_rules":[],"needs_files":[]}."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE;
    if !terse {
        return base;
    }
    let base = replace_expected(
        &base,
        r#""detail":"why it's exploitable and the fix direction"}"#,
        r#""detail":"ONE sentence, at most 25 words: why this is exploitable, naming any other file or symbol it depends on"}"#,
    );
    let base = replace_expected(&base, "  \"proposed_rules\": [],\n", "");
    let base = replace_expected(
        &base,
        r#"return {"findings":[],"proposed_rules":[],"needs_files":[]}."#,
        r#"return {"findings":[],"needs_files":[]}."#,
    );
    replace_expected(
        &base,
        AUDIT_OUTPUT_SHAPE_LEAD,
        &format!("{TERSE_DETAIL_PARAGRAPH}\n\n{AUDIT_OUTPUT_SHAPE_LEAD}"),
    )
}

/// SYSTEM PROMPT — threat-model lens.
///
/// Derives a structured threat model from the repo: entry points, trust boundaries,
/// sensitive-data paths, and the threats against them (STRIDE-flavored) with mitigations.
pub fn threat_model_system_prompt() -> String {
    r#"You are a security architect deriving a THREAT MODEL for a codebase from its structure.

Using the repo map and the file bodies provided, identify:
- ENTRY POINTS: HTTP routes/handlers, CLI commands, queue/event consumers, scheduled jobs, public APIs.
- TRUST BOUNDARIES: where untrusted input crosses into trusted code (network edge, deserialization, IPC, third-party calls).
- DATA STORES & SENSITIVE-DATA PATHS: databases, caches, file storage, secrets, and the flow of PII/credentials/financial data through them.
- DEPENDENCIES that widen the attack surface (where evident from manifests/imports).

For EACH notable element, enumerate the threats against it. Prefer STRIDE categories where they fit (spoofing, tampering, repudiation, info-disclosure, dos, elevation). Give a concrete mitigation direction and a model-inferred severity.

This is model-inferred and advisory — recall over precision; a human reviews it.

Return ONLY a JSON object, no prose, no markdown fences:
{
  "summary": "one short paragraph describing the system's attack surface",
  "threats": [
    {"component":"POST /api/orders handler","kind":"entry-point","threat":"unauthenticated order creation","category":"elevation","mitigation":"require auth + ownership check","severity":"high"}
  ]
}
`kind` is one of: "entry-point" | "trust-boundary" | "data-store" | "dependency" | "other".
If you genuinely cannot derive a model, return {"summary":"…","threats":[]}."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// Parse the SOC-2 gap-analysis lens response into `(summary, gaps)`. Robust: malformed
/// output yields an empty result rather than erroring the tier. Statuses are normalized to
/// the closed set (`met`/`partial`/`gap`/`unknown`); an unrecognized status becomes
/// `unknown` (the honest default — we did not get a clear signal).
pub fn parse_soc2_gaps(raw: &str) -> (String, Vec<Soc2Gap>) {
    let Some(json) = extract_json_object(raw) else {
        return (String::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (String::new(), Vec::new());
    };
    let summary = v["summary"].as_str().unwrap_or("").trim().to_string();
    let mut gaps = Vec::new();
    if let Some(arr) = v["gaps"].as_array() {
        for g in arr {
            let control = g["control"].as_str().unwrap_or("").trim().to_string();
            let title = g["title"].as_str().unwrap_or("").trim().to_string();
            // Drop entirely-empty rows (no control and no title — nothing to show).
            if control.is_empty() && title.is_empty() {
                continue;
            }
            let status = match g["status"].as_str().unwrap_or("unknown").trim() {
                "met" => "met",
                "partial" => "partial",
                "gap" => "gap",
                _ => "unknown",
            }
            .to_string();
            gaps.push(Soc2Gap {
                control,
                title,
                status,
                observed: g["observed"].as_str().unwrap_or("").trim().to_string(),
                gap: g["gap"].as_str().unwrap_or("").trim().to_string(),
            });
        }
    }
    (summary, gaps)
}

/// Parse the threat-model lens response into `(summary, threats)`. Robust to malformed
/// output. `kind`, `category`, and `severity` are normalized to their closed sets so the UI
/// can group on them; an unrecognized value falls back to the safest/most-generic bucket.
pub fn parse_threats(raw: &str) -> (String, Vec<Threat>) {
    let Some(json) = extract_json_object(raw) else {
        return (String::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (String::new(), Vec::new());
    };
    let summary = v["summary"].as_str().unwrap_or("").trim().to_string();
    let mut threats = Vec::new();
    if let Some(arr) = v["threats"].as_array() {
        for t in arr {
            let component = t["component"].as_str().unwrap_or("").trim().to_string();
            let threat = t["threat"].as_str().unwrap_or("").trim().to_string();
            // Need at least a component or a threat statement to be a real row.
            if component.is_empty() && threat.is_empty() {
                continue;
            }
            let kind = match t["kind"].as_str().unwrap_or("other").trim() {
                "entry-point" => "entry-point",
                "trust-boundary" => "trust-boundary",
                "data-store" => "data-store",
                "dependency" => "dependency",
                _ => "other",
            }
            .to_string();
            let severity = match t["severity"].as_str().unwrap_or("medium").trim() {
                "critical" => "critical",
                "high" => "high",
                "low" => "low",
                _ => "medium",
            }
            .to_string();
            threats.push(Threat {
                component,
                kind,
                threat,
                // Category is free-ish text; keep it verbatim (trimmed) so a STRIDE label or a
                // custom phrase both survive.
                category: t["category"].as_str().unwrap_or("").trim().to_string(),
                mitigation: t["mitigation"].as_str().unwrap_or("").trim().to_string(),
                severity,
            });
        }
    }
    (summary, threats)
}

/// Run ONE prose-style deep lens (SOC-2 gap or threat model) over the whole repo digest.
/// These two lenses are single whole-repo passes (their value is the cross-cutting view, not
/// per-chunk recall), so we build one digest, run one call, and parse the structured result.
/// Streaming into the transcript when feedback is present, so the cockpit shows the lens work
/// live. Graceful: on any model failure the lens result carries the error, never panics.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_prose_lens(
    llm: &dyn LlmPort,
    lens: DeepLens,
    repo: &str,
    repo_map: &RepoMap,
    files: &[(String, String)],
    nonce: &crate::prompt_frame::FrameNonce,
    system: String,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
) -> DeepLensResult {
    // The lens reads the whole repo. When that is more than the model's window takes it is read
    // in parts, split by files and then by line ranges (M1), each part its own request, and the
    // results are merged; nothing is cut off silently.
    let budget = request_budget::digest_budget_tokens(
        &resolve_audit_model(audit_model),
        DEEP_LENS_MAX_TOKENS,
        system.len() + repo.len() + 16 + repo_map.prefix().len() + lens.title().len() + 200,
    );
    let (parts, fit) = request_budget::fit_chunks(&[files], None, nonce, budget);
    if fit.parts_added > 0 {
        eprintln!(
            "[camerata-server] {repo} {}: the repository is over the model's context window and \
             is read in {} parts",
            lens.title(),
            parts.len()
        );
        if let Some(m) = meter {
            m.record_request_splits(crate::stage_telemetry::RequestSplits {
                chunks_split: fit.chunks_split as u64,
                parts_added: fit.parts_added as u64,
                files_split_by_lines: fit.files_split_by_lines as u64,
            });
        }
    }
    let n = parts.len();
    let mut merged = DeepLensResult::empty(lens);
    let mut errors: Vec<String> = Vec::new();
    let mut summaries: Vec<String> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let r = run_prose_part(
            llm,
            lens,
            repo,
            repo_map.prefix(),
            &part.digest,
            &system,
            (i, n),
            audit_model,
            feedback,
            meter,
        )
        .await;
        merged.soc2_gaps.extend(r.soc2_gaps);
        merged.threats.extend(r.threats);
        if !r.summary.trim().is_empty() {
            summaries.push(r.summary);
        }
        if let Some(e) = r.error {
            errors.push(if n > 1 {
                format!("part {} of {n}: {e}", i + 1)
            } else {
                e
            });
        }
    }
    merged.summary = summaries.join("\n\n");
    if n > 1 {
        let note = format!(
            "The repository is larger than the model's context window, so this lens read it in \
             {n} parts and the results are merged; a pattern that spans parts may be incomplete."
        );
        merged.summary = if merged.summary.is_empty() {
            note
        } else {
            format!("{}\n\n{note}", merged.summary)
        };
    }
    if !errors.is_empty() {
        merged.error = Some(errors.join("; "));
    }
    merged
}

/// The reply ceiling of one deep-tier lens request.
pub(super) const DEEP_LENS_MAX_TOKENS: u32 = 8192;

/// One request of a prose lens over `digest`: part `part.0` of `part.1` (a lens that fits is part
/// 0 of 1, and its prompt is exactly what it always was).
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_prose_part(
    llm: &dyn LlmPort,
    lens: DeepLens,
    repo: &str,
    repo_map: &str,
    digest: &str,
    system: &str,
    part: (usize, usize),
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
) -> DeepLensResult {
    let part_label = if part.1 > 1 {
        format!(" (part {}/{})", part.0 + 1, part.1)
    } else {
        String::new()
    };
    let prompt = format!(
        "Repository: {repo}{part_label}\n\n{repo_map}{digest}\n\n── {} for the code above. Return the JSON described in the system prompt. ──",
        lens.title()
    );
    let req = LlmRequest::new(prompt.clone())
        .with_system(system.to_string())
        // Whole-repo structured output can be sizable (many controls / many threats).
        .with_max_tokens(DEEP_LENS_MAX_TOKENS)
        .with_model(stage_model(audit_model));
    // SPEND CAP: a lens is one call, admitted like a scan pass; refused, it is reported on the
    // lens result and recorded as the deep tier stopped part way. A lens the run store already
    // holds a reply for is answered from the store and is not a send, so it is not refused.
    let _permit = if llm.replays(&req) {
        None
    } else {
        match admit_stage(meter, repo, STAGE_DEEP).await {
            Err(CapReached) => {
                let mut result = DeepLensResult::empty(lens);
                result.error =
                    Some("not run: the spend limit set for this run was reached".to_string());
                return result;
            }
            Ok(permit) => permit,
        }
    };
    let session = if part.1 > 1 {
        format!("deep-{}-{repo}-p{}", lens.id(), part.0)
    } else {
        format!("deep-{}-{repo}", lens.id())
    };
    if let Some((store, key)) = feedback {
        store.register(
            key,
            crate::transcript::AgentTranscript {
                session_id: session.clone(),
                role: format!("{} — {repo}", lens.title()),
                prompt: prompt.clone(),
                output: String::new(),
                status: "running".to_string(),
            },
        );
    }
    // Never send what the model's window cannot take (M1): refused here, disclosed on the lens.
    if let Err(e) = request_budget::ensure_request_fits(&req) {
        let mut result = DeepLensResult::empty(lens);
        result.error = Some(e.to_string());
        if let Some((store, key)) = feedback {
            store.set_status(key, &session, "blocked");
        }
        return result;
    }
    // What this request may cost joins the spend limit's reserve, as for every other stage.
    note_request(meter, &req);
    let resp = if let Some((store, key)) = feedback {
        let mut on_delta = |t: &str| store.append_output_raw(key, &session, t);
        llm.complete_streaming(req, &mut on_delta).await
    } else {
        let cap = total_backstop();
        match tokio::time::timeout(cap, llm.complete(req)).await {
            Ok(inner) => inner,
            Err(_) => Err(anyhow::anyhow!(
                "lens exceeded the {}s backstop",
                cap.as_secs()
            )),
        }
    };
    let mut result = DeepLensResult::empty(lens);
    match resp {
        Ok(r) => {
            if let Some(m) = meter {
                m.record_stage(Stage::DeepTier, &r);
            }
            match lens {
                DeepLens::Soc2Gap => {
                    let (summary, gaps) = parse_soc2_gaps(&r.text);
                    result.summary = summary;
                    result.soc2_gaps = gaps;
                }
                DeepLens::ThreatModel => {
                    let (summary, threats) = parse_threats(&r.text);
                    result.summary = summary;
                    result.threats = threats;
                }
                // Security uses the chunked path, not this prose lens.
                DeepLens::DeepSecurity => {}
            }
            if let Some((store, key)) = feedback {
                store.set_status(key, &session, "done");
            }
        }
        Err(e) => {
            meter_failed(meter, Stage::DeepTier, &e);
            result.error = Some(format!("{e}"));
            if let Some((store, key)) = feedback {
                store.set_status(key, &session, "blocked");
            }
        }
    }
    result
}

/// Run the deep SECURITY lens. It reuses the full chunked audit engine ([`run_passes`]) so a
/// large repo is covered chunk-by-chunk (the same reason the standard audit chunks), with the
/// security-focused system prompt swapped in via a single-batch free-form pass. The result is
/// the standard `Finding` shape, deduped + location-merged like the standard audit. Findings
/// are tagged `AI-`-prefixed by the parser (no adopted rules here), keeping their advisory
/// provenance honest.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_security_lens(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    repo_map: &RepoMap,
    nonce: &crate::prompt_frame::FrameNonce,
    audit_model: Option<&str>,
    mode: ScanMode,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
) -> DeepLensResult {
    let mut result = DeepLensResult::empty(DeepLens::DeepSecurity);
    if files.is_empty() {
        return result;
    }
    // The security lens has no adopted-rule corpus — it is a free-form security read — so it
    // runs as a single empty batch per chunk (one pass each), like the no-rules audit path.
    let (concurrency, _batch_size) = mode.tuning(efficiency_of(meter));
    let chunks = chunk_files(files, CHUNK_DIGEST_CHARS);
    let empty_batch: &[(String, String)] = &[];
    let batches: Vec<&[(String, String)]> = vec![empty_batch];
    let adopted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let (findings, _proposed, _requested, sec_ok, sec_failures) = run_security_passes(
        llm,
        repo,
        repo_map,
        nonce,
        &adopted,
        audit_model,
        feedback,
        &chunks,
        &batches,
        concurrency,
        meter,
    )
    .await;
    // Dedup byte-identical repeats then location-merge — same reduce the standard audit uses,
    // so one smell reported under several names at one line is ONE row.
    let mut findings = findings;
    resolve_finding_lines(&mut findings, files);
    dedup_findings_keep_max_severity(&mut findings);
    let findings = crate::review_tree::collapse_identical(
        {
            let mut findings = findings;
            crate::review_tree::mark_located(&mut findings, files);
            findings
        },
        &crate::review_tree::ToolMap::builtin(),
    )
    .rows;
    // `terse_output`: the lens asked for a one-sentence `detail`; write the full one for the
    // survivors. A lens finding that could not get one keeps the short detail plus the
    // incomplete marker, and the lens reports it in its `error` note (the deep tier's own
    // failed-pass path), so it is never dropped and never silently short.
    let mut lens_notes: Vec<String> = Vec::new();
    let findings = if terse_output_of(meter) {
        let (findings, failed) =
            regenerate_full_details(llm, repo, findings, files, &[], nonce, audit_model, meter)
                .await;
        lens_notes.extend(failed.iter().map(|f| format!("{}: {}", f.pass, f.reason)));
        findings
    } else {
        findings
    };
    result.security_findings = findings;
    // A failed (or refused) security pass must not read as "no security findings": say so on
    // the lens result, the same field the other lenses use when they fail.
    if !sec_failures.is_empty() {
        lens_notes.push(format!(
            "{} of {} security pass(es) failed, so findings from them are missing: {}",
            sec_failures.len(),
            sec_ok + sec_failures.len(),
            sec_failures
                .iter()
                .map(|f| format!("{}: {}", f.pass, f.error))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if !lens_notes.is_empty() {
        result.error = Some(lens_notes.join("; "));
    }
    result
}

/// Like [`run_passes`] but with the DEEP-SECURITY system prompt instead of the standard
/// architectural one. Kept as its own small function so the deep tier never disturbs the
/// standard audit's pass machinery, and so the security prompt is the only thing that
/// differs. Single-batch (free-form security read), so there is no rule-batch dimension.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_security_passes(
    llm: &dyn LlmPort,
    repo: &str,
    repo_map: &RepoMap,
    nonce: &crate::prompt_frame::FrameNonce,
    adopted: &std::collections::HashSet<String>,
    audit_model: Option<&str>,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    chunks: &[&[(String, String)]],
    batches: &[&[(String, String)]],
    concurrency: usize,
    meter: Option<&UsageMeter>,
) -> PassesOut {
    use futures::stream::StreamExt;
    // Chunks over the model's window are read in parts (M1), each its own request.
    let budget = request_budget::digest_budget_tokens(
        &resolve_audit_model(audit_model),
        DEEP_LENS_MAX_TOKENS,
        crate::prompt_frame::with_untrusted_clause(
            deep_security_system_prompt_with(terse_output_of(meter)),
            nonce,
        )
        .len()
            + repo.len()
            + 32
            + repo_map.prefix().len()
            + repo_map.symbols_reserve_bytes()
            + 400,
    );
    let (parts, fit) = request_budget::fit_chunks(chunks, None, nonce, budget);
    if fit.parts_added > 0 {
        eprintln!(
            "[camerata-server] {repo} deep security: {} chunk(s) were over the model's context \
             window and were split into {} more part(s)",
            fit.chunks_split, fit.parts_added
        );
        if let Some(m) = meter {
            m.record_request_splits(crate::stage_telemetry::RequestSplits {
                chunks_split: fit.chunks_split as u64,
                parts_added: fit.parts_added as u64,
                files_split_by_lines: fit.files_split_by_lines as u64,
            });
        }
    }
    let chunk_slices: Vec<&[(String, String)]> = parts.iter().map(|p| p.files).collect();
    let chunks: &[&[(String, String)]] = &chunk_slices;
    let symbols: Vec<repo_map::SymbolsSection> = chunks
        .iter()
        .map(|c| repo_map.symbols_for(c, nonce))
        .collect();
    let n_c = chunks.len();
    let n_b = batches.len().max(1);
    let work: Vec<usize> = (0..n_c).collect();
    type PassOut = (
        usize,
        anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)>,
    );
    let results: Vec<PassOut> = futures::stream::iter(work)
        .map(|ci| {
            let digest = chunk_segment(&symbols[ci].text, &parts[ci].digest);
            let repo_map = repo_map.prefix();
            async move {
                let prompt = format!(
                    "Repository: {repo} (security pass {}/{n_c})\n\n{repo_map}{digest}\n\n── Perform a DEEP SECURITY AUDIT of the code above. Use the REPO MAP for cross-file context. Return the JSON described in the system prompt. ──",
                    ci + 1,
                );
                // The security lens swaps in its own system prompt; everything else mirrors
                // `audit_pass` (streaming + meter + robust parse).
                let req = LlmRequest::new(prompt.clone())
                    .with_system(crate::prompt_frame::with_untrusted_clause(
                        deep_security_system_prompt_with(terse_output_of(meter)),
                        nonce,
                    ))
                    .with_max_tokens(DEEP_LENS_MAX_TOKENS)
                    .with_temperature(AUDIT_TEMPERATURE)
                    .with_model(stage_model(audit_model));
                // SPEND CAP: each deep-security pass is admitted like a scan pass, unless the run
                // store already holds its reply (then it is not a send).
                let _permit = if llm.replays(&req) {
                    None
                } else {
                    match admit_stage(meter, repo, STAGE_DEEP).await {
                        Err(CapReached) => {
                            return (
                                ci,
                                Err(anyhow::anyhow!(
                                    "not run: the spend limit set for this run was reached"
                                )),
                            );
                        }
                        Ok(permit) => permit,
                    }
                };
                let session = format!("deep-security-{repo}-c{ci}");
                if let Some((store, key)) = feedback {
                    store.register(
                        key,
                        crate::transcript::AgentTranscript {
                            session_id: session.clone(),
                            role: format!("Deep Security Audit {}/{n_c} — {repo}", ci + 1),
                            prompt: prompt.clone(),
                            output: String::new(),
                            status: "running".to_string(),
                        },
                    );
                }
                let r: anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)> = async {
                    // Never send what the model's window cannot take (M1).
                    request_budget::ensure_request_fits(&req)?;
                    // What this request may cost joins the spend limit's reserve.
                    note_request(meter, &req);
                    let call = if let Some((store, key)) = feedback {
                        let mut on_delta = |t: &str| store.append_output_raw(key, &session, t);
                        llm.complete_streaming(req, &mut on_delta).await
                    } else {
                        let cap = total_backstop();
                        tokio::time::timeout(cap, llm.complete(req))
                            .await
                            .map_err(|_| anyhow::anyhow!("security pass exceeded the {}s backstop", cap.as_secs()))
                            .and_then(|r| r)
                    };
                    let resp = match call {
                        Ok(r) => r,
                        Err(e) => {
                            meter_failed(meter, Stage::DeepTier, &e);
                            return Err(e);
                        }
                    };
                    if let Some(m) = meter {
                        m.record_stage(Stage::DeepTier, &resp);
                    }
                    // A complete reply that holds no JSON is a FAILED pass, never "no findings":
                    // parsing it would return an empty list indistinguishable from a clean
                    // chunk. (No retry here: the deep tier is a single opt-in pass per chunk.)
                    if !resp.truncated && scan_output_is_malformed(&resp.text) {
                        if let Some(m) = meter {
                            m.record_wasted(Stage::DeepTier, &resp);
                        }
                        anyhow::bail!("{}", malformed_reply_reason(1));
                    }
                    // Cut off at the output limit: the complete findings that arrived are kept
                    // and the chunk is reported as not fully reviewed (H1).
                    if resp.truncated {
                        if let Some(m) = meter {
                            m.record_wasted(Stage::DeepTier, &resp);
                        }
                        return Err(
                            PartialPassReply::from_cut_off(repo, &resp.text, adopted, 1).into()
                        );
                    }
                    let (f, p) = parse_ai_findings(repo, &resp.text, adopted);
                    let needs = parse_needs_files(&resp.text);
                    Ok((f, p, needs))
                }
                .await;
                if let Some((store, key)) = feedback {
                    store.set_status(key, &session, if r.is_ok() { "done" } else { "blocked" });
                }
                (ci, r)
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await;

    let mut findings = Vec::new();
    let mut proposed = Vec::new();
    let mut requested = std::collections::HashSet::new();
    let mut ok = 0usize;
    let mut failures: Vec<PassFailure> = Vec::new();
    for (ci, r) in results {
        match r {
            Ok((f, p, needs)) => {
                findings.extend(f);
                proposed.extend(p);
                requested.extend(needs);
                ok += 1;
            }
            Err(e) => {
                let failure = PassFailure::named(format!("deep security {}/{n_c}", ci + 1), &e)
                    .with_files(chunks[ci].len());
                match e.downcast::<PartialPassReply>() {
                    Ok(partial) => {
                        failures.push(failure.with_recovered(partial.findings.len()));
                        findings.extend(partial.findings);
                        proposed.extend(partial.proposed);
                        requested.extend(partial.needs);
                    }
                    Err(_) => failures.push(failure),
                }
            }
        }
    }
    let _ = n_b; // single batch; kept for parity/readability with run_passes.
    (findings, proposed, requested, ok, failures)
}

/// Run the full DEEP COMPLIANCE & SECURITY tier (#55) over one repo's files: the three lenses
/// (SOC-2 gap analysis, deep security audit, threat model), each on the selected/Opus model.
/// ADDITIVE and OPT-IN — only called when the audit request set `deep`; the standard scan is
/// untouched. Every lens is best-effort: a failure attaches an `error` to that lens's result
/// and the others still run. Spend folds into the shared [`UsageMeter`].
///
/// `mode` controls the security lens's chunk concurrency (the prose lenses are single passes).
///
/// `soc2_enabled` gates the SOC-2 gap-analysis lens (feature flag). When `false`, ONLY the
/// soc2 lens is skipped — deep-security and threat-model still run and the report is valid with
/// an empty `soc2_gaps` field. The SOC-2 code is NOT removed; the lens is skipped at call time.
/// Pass `true` (the flag default) to run all three lenses as before.
#[allow(clippy::too_many_arguments)]
pub async fn run_deep_tier(
    llm: &dyn LlmPort,
    repo: &str,
    files: &[(String, String)],
    audit_model: Option<&str>,
    mode: ScanMode,
    feedback: Option<(&crate::transcript::TranscriptStore, &str)>,
    meter: Option<&UsageMeter>,
    soc2_enabled: bool,
) -> DeepReport {
    // The deep tier reads the whole repo: redact before any lens sees it.
    let model_files = crate::onboard::redact::files_for_model(files);
    let files = model_files.as_slice();
    // One delimiter token for all three lenses (see `crate::prompt_frame`).
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding_files(&[files]);
    // Resolve the model the same way the standard audit does: explicit pick wins, else the
    // built-in default (the transport has no default of its own: see `stage_model`). The deep tier is meant to run on the strong (Opus) model; the caller
    // passes that through `audit_model`.
    let model = Some(resolve_audit_model(audit_model));
    // The same map as the standard audit's: the complete file tree, with symbols per call.
    let repo_map = RepoMap::new(
        files,
        &nonce,
        repo_map::tree_budget_for_window(request_budget::context_window_tokens(
            model.as_deref().unwrap_or_default(),
        )),
    );

    let threat = run_prose_lens(
        llm,
        DeepLens::ThreatModel,
        repo,
        &repo_map,
        files,
        &nonce,
        crate::prompt_frame::with_untrusted_clause(threat_model_system_prompt(), &nonce),
        model.as_deref(),
        feedback,
        meter,
    );
    let security = run_security_lens(
        llm,
        repo,
        files,
        &repo_map,
        &nonce,
        model.as_deref(),
        mode,
        feedback,
        meter,
    );

    if soc2_enabled {
        // All three lenses run concurrently.
        let soc2 = run_prose_lens(
            llm,
            DeepLens::Soc2Gap,
            repo,
            &repo_map,
            files,
            &nonce,
            crate::prompt_frame::with_untrusted_clause(soc2_gap_system_prompt(), &nonce),
            model.as_deref(),
            feedback,
            meter,
        );
        let (soc2, security, threat) = tokio::join!(soc2, security, threat);
        DeepReport {
            // Stable order: gap analysis, security, threat model.
            lenses: vec![soc2, security, threat],
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
        }
    } else {
        // SOC-2 lens disabled by feature flag: run only security + threat-model.
        // The report is still valid; soc2_gaps will be empty on all lenses.
        let (security, threat) = tokio::join!(security, threat);
        DeepReport {
            // Stable order maintained: security, threat model (soc2 omitted).
            lenses: vec![security, threat],
            advisory: true,
            disclaimer: DEEP_ADVISORY_DISCLAIMER.to_string(),
        }
    }
}
