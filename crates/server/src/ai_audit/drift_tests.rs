//! H5 item 3, drift reduction in the audit itself: the passes that decide WHICH findings exist
//! ask for temperature 0 (sent only where the model accepts it, see
//! `camerata_llm::model_registry::accepts_sampling_params`), and the same files and rules
//! handed over in a different order produce the same prompts.

use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::scan_routing::AuditRule;

/// Records every request and answers each with one finding, so the scan, calibration and
/// follow-on passes all run.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmPort for Recorder {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.seen.lock().unwrap().push(req);
        Ok(LlmResponse {
            text: r#"{"findings":[{"rule":"ARCH-NO-DIRECT-DB-1","severity":"high","path":"src/a.rs","code":"db.execute(query)","title":"direct DB call","detail":"controller calls the database directly"}],"verdicts":[]}"#
                .to_string(),
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn files() -> Vec<(String, String)> {
    vec![
        (
            "src/a.rs".to_string(),
            "fn handler() { db.execute(query); }\n".to_string(),
        ),
        (
            "src/b.rs".to_string(),
            "fn helper() -> u32 { 1 }\n".to_string(),
        ),
        ("lib/c.rs".to_string(), "pub mod util;\n".to_string()),
    ]
}

fn rules() -> Vec<AuditRule> {
    vec![
        AuditRule::new(
            "ARCH-NO-DIRECT-DB-1",
            "Controllers must not call the database directly.",
        ),
        AuditRule::new(
            "ARCH-STRICT-LAYERING-1",
            "Each layer only calls the one below it.",
        ),
    ]
}

/// Does the request's system prompt start with `base`? Every pass appends the untrusted-content
/// clause (which names the run's random delimiter token) after its own prompt.
fn is_system(r: &LlmRequest, base: &str) -> bool {
    r.system.as_deref().is_some_and(|s| s.starts_with(base))
}

/// The request's prompt with the run's delimiter token replaced by a constant, so two runs that
/// drew different tokens can be compared byte for byte.
fn without_token(r: &LlmRequest) -> String {
    let system = r.system.clone().unwrap_or_default();
    let token: String = system
        .split_once("delimiter token is ")
        .map(|(_, rest)| rest.chars().take_while(char::is_ascii_hexdigit).collect())
        .unwrap_or_default();
    if token.is_empty() {
        r.prompt.clone()
    } else {
        r.prompt.replace(&token, "TOKEN")
    }
}

async fn audit(files: &[(String, String)], rules: &[AuditRule]) -> Vec<LlmRequest> {
    let llm = Recorder::default();
    audit_repo(
        &llm,
        "acme/api",
        files,
        rules,
        &[],
        &std::collections::HashMap::new(),
        Some("claude-haiku-4-5-20251001"),
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("the recorder never errors");
    llm.seen.into_inner().unwrap()
}

#[tokio::test]
async fn the_scan_and_calibration_passes_ask_for_temperature_zero() {
    let seen = audit(&files(), &rules()).await;
    let scan: Vec<&LlmRequest> = seen
        .iter()
        .filter(|r| is_system(r, &audit_system_prompt()))
        .collect();
    let calibration: Vec<&LlmRequest> = seen
        .iter()
        .filter(|r| is_system(r, &verify_system_prompt()))
        .collect();
    assert!(
        !scan.is_empty() && !calibration.is_empty(),
        "both passes ran"
    );
    for r in scan.iter().chain(calibration.iter()) {
        assert_eq!(r.temperature, Some(AUDIT_TEMPERATURE), "{:?}", r.system);
    }
    assert_eq!(AUDIT_TEMPERATURE, 0.0);
}

#[tokio::test]
async fn the_same_files_and_rules_in_another_order_produce_the_same_prompts() {
    let mut shuffled_files = files();
    shuffled_files.reverse();
    let mut shuffled_rules = rules();
    shuffled_rules.reverse();

    let prompts = |seen: Vec<LlmRequest>| {
        let mut p: Vec<String> = seen
            .into_iter()
            .filter(|r| is_system(r, &audit_system_prompt()))
            .map(|r| without_token(&r))
            .collect();
        // Parallel passes may finish in any order; what is sent must not depend on input order.
        p.sort();
        p
    };
    let a = prompts(audit(&files(), &rules()).await);
    let b = prompts(audit(&shuffled_files, &shuffled_rules).await);
    assert!(!a.is_empty());
    assert_eq!(a, b, "input order changed what the model was asked");
}
