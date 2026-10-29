//! Tests for construction-time redaction (docs/design/2026-10-23_secret-redaction-and-release-
//! gate.md section 8.3 and 8.4). Every secret is built at runtime by
//! `camerata_secrets::fixtures`; no secret-shaped literal appears here.

use camerata_api_types::audit::{
    ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind, NotAssessedKind,
    NotAssessedReason, ObservedOptionUsage,
};
use camerata_secrets::fixtures::{planted, planted_all, Planted};
use camerata_secrets::{detect_secrets, SecretShape};

use super::*;
use crate::onboard::{Finding, MergedLocation, ScanReport};

/// The shapes the sweep exports and the work order name.
const WIRE_SHAPES: &[SecretShape] = &[
    SecretShape::SupabaseSecretKey,
    SecretShape::SupabaseServiceRoleJwt,
    SecretShape::StripeLiveSecretKey,
    SecretShape::StripeWebhookSecret,
    SecretShape::PemPrivateKey,
    SecretShape::NamedSecretLiteral,
    SecretShape::ConnectionStringPassword,
];

/// Whether any line of the planted value appears in `haystack` (a multi-line PEM body leaks if
/// any one of its lines does).
fn leaked(haystack: &str, p: &Planted) -> bool {
    p.value.lines().any(|line| haystack.contains(line))
}

fn kinds_carrying(text: &str) -> Vec<FindingKind> {
    vec![
        FindingKind::Defect,
        FindingKind::Deviation(DeviationKind {
            deviating_sites: 2,
            assessed_sites: Some(9),
            convention_option_id: None,
            convention_label: text.to_string(),
        }),
        FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: "opt".to_string(),
            convention_label: text.to_string(),
        }),
        FindingKind::NoConvention(NoConventionKind {
            observed_options: vec![ObservedOptionUsage {
                option_id: "opt".to_string(),
                label: text.to_string(),
                sites: Some(1),
            }],
        }),
        FindingKind::NotAssessed(NotAssessedKind {
            reason: NotAssessedReason::ToolFailed,
            reason_detail: text.to_string(),
        }),
    ]
}

fn finding_carrying(p: &Planted, kind: FindingKind) -> Finding {
    let mut captures = std::collections::BTreeMap::new();
    captures.insert("object".to_string(), p.text.clone());
    Finding {
        repo: "acme/app".to_string(),
        path: ".env".to_string(),
        line: 3,
        rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
        severity: "critical".to_string(),
        snippet: p.text.clone(),
        detail: format!("The line `{}` holds a credential.", p.text),
        fix_specific: Some(format!(
            "Replace `{}` with an environment variable.",
            p.text
        )),
        calibration_rationale: Some(format!("Floored: {}", p.text)),
        captures,
        also_locations: vec![MergedLocation::site(
            "acme/app".to_string(),
            ".env.local".to_string(),
            5,
            "SEC-NO-VENDOR-TOKEN-1".to_string(),
            p.text.clone(),
            false,
        )],
        kind: Some(kind),
        ..Finding::default()
    }
}

#[test]
fn redact_finding_covers_every_client_visible_text_field_for_each_shape() {
    for shape in WIRE_SHAPES {
        let p = planted(*shape);
        for kind in kinds_carrying(&p.text) {
            let mut f = finding_carrying(&p, kind);
            let count = redact_finding(&mut f);
            let json = serde_json::to_string(&f).expect("a finding serializes");
            assert!(count > 0, "{shape:?} must be reported as redacted");
            assert!(
                !leaked(&json, &p),
                "{shape:?} leaked in {}",
                f.kind.as_ref().map_or("none", |k| k.tag())
            );
            assert!(detect_secrets(&f.snippet).is_empty());
            assert!(detect_secrets(&f.detail).is_empty());
            assert!(f
                .fix_specific
                .as_deref()
                .is_some_and(|t| detect_secrets(t).is_empty()));
            assert!(f
                .calibration_rationale
                .as_deref()
                .is_some_and(|t| detect_secrets(t).is_empty()));
            assert!(f.captures.values().all(|v| detect_secrets(v).is_empty()));
            assert!(f
                .also_locations
                .iter()
                .all(|l| detect_secrets(&l.snippet).is_empty()));
        }
    }
}

#[test]
fn redact_finding_leaves_the_redacted_form_in_place_of_the_value() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let mut f = finding_carrying(&p, FindingKind::Defect);
    redact_finding(&mut f);
    assert!(f.snippet.contains("sb_secret_…"), "snippet: {}", f.snippet);
    assert!(
        f.snippet.starts_with("SUPABASE_KEY="),
        "the surrounding syntax stays"
    );
    assert!(f.detail.contains("sb_secret_…"));
    assert!(f.also_locations[0].snippet.contains("(51 chars)"));
}

#[test]
fn redact_finding_is_idempotent() {
    for p in planted_all() {
        let mut f = finding_carrying(&p, FindingKind::Defect);
        redact_finding(&mut f);
        let once = f.clone();
        assert_eq!(
            redact_finding(&mut f),
            0,
            "{:?} re-redacted its own form",
            p.shape
        );
        assert_eq!(f, once);
    }
}

#[test]
fn redact_finding_leaves_a_clean_finding_untouched_and_counts_zero() {
    let mut f = Finding {
        snippet: "let x = compute(y);".to_string(),
        detail: "An ordinary finding.".to_string(),
        ..Finding::default()
    };
    let before = f.clone();
    assert_eq!(redact_finding(&mut f), 0);
    assert_eq!(f, before);
}

#[test]
fn redact_findings_covers_a_whole_slice_and_sums_the_count() {
    let a = planted(SecretShape::StripeLiveSecretKey);
    let b = planted(SecretShape::GitHubToken);
    let mut fs = vec![
        finding_carrying(&a, FindingKind::Defect),
        Finding::default(),
        finding_carrying(&b, FindingKind::Defect),
    ];
    let count = redact_findings(&mut fs);
    assert!(count >= 2);
    for f in &fs {
        let json = serde_json::to_string(f).expect("serializes");
        assert!(!leaked(&json, &a) && !leaked(&json, &b));
    }
}

#[test]
fn snippet_from_line_redacts_before_the_cap() {
    // The secret starts at char 141, so a 160-char cap applied FIRST would keep its first 19
    // chars: a prefix plus a partial body the detector cannot recognise.
    let p = planted(SecretShape::StripeLiveSecretKey);
    let line = format!("  {} {}", "z".repeat(138), p.value);
    let capped_first: String = line.trim().chars().take(160).collect();
    assert!(
        capped_first.contains(&p.value[..19]),
        "precondition: capping first would leave a secret fragment"
    );
    let snippet = snippet_from_line(&line, 160);
    assert!(snippet.chars().count() <= 160);
    assert!(
        !snippet.contains(&p.value[..19]),
        "a fragment of the key survived: {snippet}"
    );
    assert!(detect_secrets(&snippet).is_empty());
    assert!(
        snippet.contains("sk_live_"),
        "the redacted form carries the prefix"
    );
}

#[test]
fn snippet_from_line_trims_and_leaves_a_clean_line_alone() {
    assert_eq!(snippet_from_line("    let x = 1;   ", 160), "let x = 1;");
    let long = "y".repeat(300);
    assert_eq!(snippet_from_line(&long, 160).chars().count(), 160);
}

fn report_with_text(p: &Planted) -> ScanReport {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    let rec = crate::ai_audit::RuleRecommendation {
        repo: String::new(),
        rule_id: "ARCH-X-1".to_string(),
        recommended_option_id: "a".to_string(),
        recommendation_reasoning: format!("because {}", p.text),
        evidence: Some(format!("src/a.ts:3 {}", p.text)),
        observed_options: vec![ObservedOptionUsage {
            option_id: "a".to_string(),
            label: p.text.clone(),
            sites: None,
        }],
        ..crate::ai_audit::RuleRecommendation::default()
    };
    // The per-repo list holds the same text, one entry per (rule, repo).
    report
        .recommendations_by_repo
        .push(crate::ai_audit::RuleRecommendation {
            repo: "acme/app".to_string(),
            ..rec.clone()
        });
    report.recommendations.insert("ARCH-X-1".to_string(), rec);
    report.coverage_notes.push(crate::onboard::CoverageNote {
        tool_failed: false,
        repo: None,
        tool: "eslint".to_string(),
        message: format!("tool output: {}", p.text),
    });
    report.failed_passes.push(crate::ai_audit::FailedPass {
        repo: "acme/app".to_string(),
        pass: "alternatives".to_string(),
        reason: format!("error: {}", p.text),
        files_not_reviewed: 0,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: false,
    });
    report.ai_error = Some(format!("auth failed {}", p.text));
    report.ai_blocked_reason = Some(p.text.clone());
    report.message = Some(p.text.clone());
    report
}

#[test]
fn redact_report_text_covers_every_report_level_string() {
    for shape in WIRE_SHAPES {
        let p = planted(*shape);
        let mut report = report_with_text(&p);
        let count = redact_report_text(&mut report);
        assert!(count > 0, "{shape:?}");
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!leaked(&json, &p), "{shape:?} leaked in report-level text");
        assert_eq!(redact_report_text(&mut report), 0, "idempotent");
    }
}

#[test]
fn files_for_model_redacts_every_shape_and_keeps_line_counts() {
    let mut files = vec![("src/clean.rs".to_string(), "fn main() {}\n".to_string())];
    for p in planted_all() {
        files.push((
            format!("src/{:?}.txt", p.shape),
            format!("line one\n{}\nline three\n", p.text),
        ));
    }
    let model = files_for_model_with(&files, true);
    assert_eq!(model.len(), files.len());
    for ((raw_path, raw), (path, content)) in files.iter().zip(&model) {
        assert_eq!(raw_path, path, "paths are unchanged");
        assert_eq!(
            raw.matches('\n').count(),
            content.matches('\n').count(),
            "line count changed for {path}"
        );
        assert!(
            detect_secrets(content).is_empty(),
            "{path} still holds a secret"
        );
    }
    assert_eq!(model[0].1, files[0].1, "a clean file is returned unchanged");
    for (p, (_, content)) in planted_all().iter().zip(model.iter().skip(1)) {
        assert!(
            !leaked(content, p),
            "{:?} reached the model prompt",
            p.shape
        );
    }
}

#[test]
fn model_prompt_redaction_flag_off_sends_raw() {
    let p = planted(SecretShape::GitHubToken);
    let files = vec![("a.env".to_string(), p.text.clone())];
    let raw = files_for_model_with(&files, false);
    assert_eq!(raw, files, "flag off: the raw set goes to the model");
    let redacted = files_for_model_with(&files, true);
    assert_ne!(redacted, files);
}

#[test]
#[serial_test::serial(process_env)]
fn files_for_model_reads_the_feature_flag_from_the_environment() {
    let p = planted(SecretShape::GitHubToken);
    let files = vec![("a.env".to_string(), p.text.clone())];
    assert_ne!(files_for_model(&files), files, "default ON");
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_FEATURE_MODEL_PROMPT_REDACTION", "false");
    assert_eq!(files_for_model(&files), files, "operator switched it off");
}

#[test]
fn fingerprint_and_finding_key_are_computed_from_the_redacted_snippet_and_stable() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let content = format!("# config\n{}\n", p.text);
    let first = crate::onboard::audit_content("acme/app", ".env.production", &content);
    let second = crate::onboard::audit_content("acme/app", ".env.production", &content);
    assert!(!first.is_empty(), "the floor must still find the secret");
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(
            crate::suppression::fingerprint(&a.rule_id, &a.snippet),
            crate::suppression::fingerprint(&b.rule_id, &b.snippet),
            "stable across two scans of the same file"
        );
        assert_eq!(
            crate::report_export::finding_key(a),
            crate::report_export::finding_key(b)
        );
        assert!(
            !a.snippet.contains(&p.value),
            "the key hashes the REDACTED snippet"
        );
        assert_ne!(
            crate::suppression::fingerprint(&a.rule_id, &a.snippet),
            crate::suppression::fingerprint(&a.rule_id, &p.text),
            "the fingerprint is not the raw line's"
        );
    }
}

#[test]
fn seal_redacts_any_finding_a_producer_missed() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let mut report = report_with_text(&p);
    report
        .findings
        .push(finding_carrying(&p, FindingKind::Defect));
    let sealed = report.seal();
    let json = serde_json::to_string(&*sealed).expect("serializes");
    assert!(
        !leaked(&json, &p),
        "the seal sweep is the belt-and-braces catch"
    );
}

#[test]
fn unredacted_count_at_seal_is_reported_so_a_missing_producer_is_visible() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let mut report = report_with_text(&p);
    report
        .findings
        .push(finding_carrying(&p, FindingKind::Defect));
    let missed = report.redact_before_seal();
    assert!(
        missed > 0,
        "the sweep reports how many values reached the seal raw"
    );
    assert_eq!(
        report.redact_before_seal(),
        0,
        "a clean report reports zero"
    );
}

// Log capture. The first version installed a scoped subscriber per call, which raced with every
// other test thread: `tracing` caches whether a callsite is enabled in a process-global table, so
// a callsite another test hit while this test's subscriber was not yet (or no longer) registered
// could stay cached as "disabled", and the warning was silently dropped. Here ONE subscriber is
// installed for the whole test binary, once, so the callsite interest never changes; each
// `logged_while` records only the events emitted on its own thread, into a thread-local buffer.

thread_local! {
    /// The events being captured on this thread, or `None` when no `logged_while` is running.
    static CAPTURED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Appends an event's fields (`message` first) to the current thread's capture buffer.
struct CaptureLayer;

struct FieldText<'a>(&'a mut String);

impl tracing::field::Visit for FieldText<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        CAPTURED.with(|captured| {
            if let Some(buffer) = captured.borrow_mut().as_mut() {
                event.record(&mut FieldText(buffer));
                buffer.push('\n');
            }
        });
    }
}

fn install_capture_once() {
    use tracing_subscriber::layer::SubscriberExt as _;
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let subscriber = tracing_subscriber::registry()
            .with(CaptureLayer)
            .with(tracing::level_filters::LevelFilter::WARN);
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other test in this binary installs a global tracing subscriber");
    });
}

/// Run `f` and return its result with the WARN-and-above events it emitted on this thread.
fn logged_while<T>(f: impl FnOnce() -> T) -> (T, String) {
    install_capture_once();
    CAPTURED.with(|captured| *captured.borrow_mut() = Some(String::new()));
    let out = f();
    let text = CAPTURED
        .with(|captured| captured.borrow_mut().take())
        .unwrap_or_default();
    (out, text)
}

#[test]
fn seal_warns_once_when_it_had_to_redact_and_never_logs_the_value() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report
        .findings
        .push(finding_carrying(&p, FindingKind::Defect));
    let (_, log) = logged_while(|| report.seal());
    assert_eq!(
        log.matches("reached seal unredacted").count(),
        1,
        "exactly one warning per seal: {log}"
    );
    assert!(!leaked(&log, &p), "the warning must not echo the value");
}

#[test]
fn seal_is_silent_when_every_producer_redacted() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let mut f = finding_carrying(&p, FindingKind::Defect);
    redact_finding(&mut f);
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.findings.push(f);
    let (_, log) = logged_while(|| report.seal());
    assert!(!log.contains("reached seal unredacted"), "{log}");
}

#[test]
fn flag_off_logs_a_warning_that_raw_code_is_leaving() {
    let files = vec![("a.txt".to_string(), "x".to_string())];
    let (_, log) = logged_while(|| files_for_model_with(&files, false));
    assert!(log.contains("model_prompt_redaction is OFF"), "{log}");
    let (_, quiet) = logged_while(|| files_for_model_with(&files, true));
    assert!(quiet.is_empty(), "{quiet}");
}

// ---- leak hunt: zero-width, the ledger, the deep report ---------------------------------------

/// `value` with a zero-width space inserted inside its prefix, so the raw string no longer
/// matches any shape while a reader still sees the key.
fn with_zero_width_inside(value: &str) -> String {
    format!("{}\u{200B}{}", &value[..2], &value[2..])
}

#[test]
fn redact_finding_strips_zero_width_so_an_inserted_one_cannot_hide_a_key() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let hidden = with_zero_width_inside(&p.value);
    assert!(
        detect_secrets(&hidden).is_empty(),
        "precondition: the zero-width char hides the key from the detector"
    );
    let mut f = Finding {
        snippet: format!("KEY={hidden}"),
        detail: format!("The line `KEY={hidden}` holds a credential."),
        fix_specific: Some(format!("Move {hidden} to an env var.")),
        ..Finding::default()
    };
    let count = redact_finding(&mut f);
    assert!(count >= 3, "all three fields redacted, got {count}");
    let json = serde_json::to_string(&f).expect("serializes");
    assert!(!leaked(&json, &p), "{json}");
    assert!(
        !json.contains('\u{200B}'),
        "the invisible character is gone"
    );
    assert!(f.snippet.contains("sk_live_…"), "{}", f.snippet);
}

#[test]
fn files_for_model_strips_zero_width_so_an_inserted_one_cannot_hide_a_key() {
    let p = planted(SecretShape::GitHubToken);
    let files = vec![(
        "a.env".to_string(),
        format!("GITHUB_TOKEN={}\n", with_zero_width_inside(&p.value)),
    )];
    let model = files_for_model_with(&files, true);
    assert!(!leaked(&model[0].1, &p), "{}", model[0].1);
    assert!(!model[0].1.contains('\u{200B}'));
    assert_eq!(model[0].1.matches('\n').count(), 1, "line count unchanged");
}

#[test]
fn snippet_from_line_strips_zero_width() {
    let p = planted(SecretShape::GitHubToken);
    let line = format!("token = {}", with_zero_width_inside(&p.value));
    let snippet = snippet_from_line(&line, 160);
    assert!(!leaked(&snippet, &p), "{snippet}");
}

#[test]
fn redact_report_text_redacts_the_scan_ledger_notes_before_they_are_persisted() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let entry = serde_json::json!({
        "rule_id": "ARCH-X-1",
        "tier": "semantic",
        "ran": false,
        "skip_reason": format!("tool said: {}", p.text),
        "files_evaluated": 0,
        "findings_emitted": 0,
        "coverage_note": format!("coverage: {}", p.text),
    });
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.ledger = serde_json::from_value(serde_json::json!({
        "rules": {"ARCH-X-1": entry},
        "stages": [],
    }))
    .expect("a ledger deserializes");
    let count = redact_report_text(&mut report);
    assert_eq!(count, 2, "the skip reason and the coverage note");
    let persisted = serde_json::to_string(&report.ledger).expect("serializes");
    assert!(!leaked(&persisted, &p), "{persisted}");
    let note = report
        .ledger
        .rule("ARCH-X-1")
        .and_then(|e| e.coverage_note.clone())
        .unwrap_or_default();
    assert!(note.contains("sk_live_…"), "{note}");
    assert_eq!(redact_report_text(&mut report), 0, "idempotent");
}

fn deep_report_carrying(p: &Planted) -> crate::ai_audit::DeepReport {
    use crate::ai_audit::{DeepLensResult, DeepReport, Soc2Gap, Threat};
    let text = p.text.clone();
    let lens = |id: &str| DeepLensResult {
        lens: id.to_string(),
        title: id.to_string(),
        advisory: true,
        disclaimer: "advisory".to_string(),
        soc2_gaps: vec![Soc2Gap {
            control: "CC6.1".to_string(),
            title: format!("Access control {text}"),
            status: "gap".to_string(),
            observed: format!("found {text}"),
            gap: format!("remove {text}"),
        }],
        security_findings: Vec::new(),
        threats: vec![Threat {
            component: format!("handler {text}"),
            kind: "entry-point".to_string(),
            threat: format!("leak of {text}"),
            category: "info-disclosure".to_string(),
            mitigation: format!("rotate {text}"),
            severity: "high".to_string(),
        }],
        summary: format!("The repo commits {text}."),
        error: Some(format!("lens failed near {text}")),
    };
    DeepReport {
        lenses: vec![
            lens("soc2-gap"),
            lens("deep-security"),
            lens("threat-model"),
        ],
        advisory: true,
        disclaimer: format!("advisory {text}"),
    }
}

#[test]
fn the_deep_report_export_redacts_the_lens_prose_it_reads_from_the_job_store() {
    for shape in WIRE_SHAPES {
        let p = planted(*shape);
        let markdown = crate::deep_report_markdown(deep_report_carrying(&p), true);
        assert!(markdown.contains("Deep Compliance"), "it still renders");
        assert!(!leaked(&markdown, &p), "{shape:?} leaked in the export");
        assert!(
            detect_secrets(&markdown).is_empty(),
            "{shape:?} still matches a shape"
        );
    }
}

#[test]
fn redact_deep_report_covers_every_model_written_string_and_is_idempotent() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let mut deep = deep_report_carrying(&p);
    let count = redact_deep_report(&mut deep);
    assert!(count > 0);
    let json = serde_json::to_string(&serde_json::json!({
        "lenses": deep.lenses.iter().map(|l| (&l.summary, &l.error, &l.soc2_gaps, &l.threats)).collect::<Vec<_>>(),
    }))
    .expect("serializes");
    assert!(!leaked(&json, &p), "{json}");
    assert_eq!(redact_deep_report(&mut deep), 0, "idempotent");
}
