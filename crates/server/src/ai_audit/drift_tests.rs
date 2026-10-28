//! H5 item 3, drift reduction in the audit itself: the passes that decide WHICH findings exist
//! ask for temperature 0 (sent only where the model accepts it, see
//! `camerata_llm::model_registry::accepts_sampling_params`), and the same files and rules
//! handed over in a different order produce the same prompts.

use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

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

fn rules() -> Vec<(String, String)> {
    vec![
        (
            "ARCH-NO-DIRECT-DB-1".to_string(),
            "Controllers must not call the database directly.".to_string(),
        ),
        (
            "ARCH-STRICT-LAYERING-1".to_string(),
            "Each layer only calls the one below it.".to_string(),
        ),
    ]
}

async fn audit(files: &[(String, String)], rules: &[(String, String)]) -> Vec<LlmRequest> {
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
        .filter(|r| r.system.as_deref() == Some(audit_system_prompt().as_str()))
        .collect();
    let calibration: Vec<&LlmRequest> = seen
        .iter()
        .filter(|r| r.system.as_deref() == Some(verify_system_prompt().as_str()))
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
            .filter(|r| r.system.as_deref() == Some(audit_system_prompt().as_str()))
            .map(|r| r.prompt)
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
