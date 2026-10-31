//! Retry fixes F3, item 6: the fix checker grounds a name in the finding's evidence window AND in
//! the repository. A name that exists in the repository (a column two screens down, `tsconfig.json`,
//! `.env.example`, a helper in another file) is not invented; a name found nowhere in the
//! repository still is. No network.

use super::structured_output_tests::{files as work_files, finding};
use super::*;

fn repo_files() -> Vec<(String, String)> {
    let mut files = work_files();
    files.push((
        "lib/audit.ts".to_string(),
        "export function writeAudit(actor_id: string, bucket_id: string) {\n  return insert(actor_id, bucket_id);\n}\n".to_string(),
    ));
    files.push(("tsconfig.json".to_string(), "{}\n".to_string()));
    files.push((".env.example".to_string(), "SUPABASE_URL=\n".to_string()));
    files.push((
        "supabase/migrations/0005_billing.sql".to_string(),
        "create table public.billing_accounts (user_id uuid primary key);\n".to_string(),
    ));
    files
}

/// The finding's own window: nothing of `lib/audit.ts`, `tsconfig.json` or `.env.example`.
const WINDOW: &str = "src/work.ts doWork(raw) runWork passes raw to doWork";

/// Names that exist elsewhere in the repository are grounded; the evidence alone does not hold
/// them.
#[test]
fn a_name_that_exists_in_the_repository_is_not_invented() {
    let files = repo_files();
    let repo = RepoNames::new(&files);
    for fix in [
        "Pass `actor_id` to the audit writer.",
        "Record it with `writeAudit(actor_id, bucket_id)` before the write.",
        "Turn the option on in tsconfig.json for this folder.",
        "Copy the variables from `.env.example` into the deploy settings.",
        "Add the check to `lib/audit.ts`.",
        "Add a policy on the `billing_accounts` table.",
    ] {
        assert!(
            find_ungrounded_identifier(fix, WINDOW).is_some(),
            "the window alone does not hold a name of: {fix}"
        );
        assert_eq!(
            find_ungrounded_identifier_in(fix, WINDOW, Some(&repo)),
            None,
            "{fix}"
        );
    }
}

/// A name found nowhere in the repository is still rejected, in prose, in a snippet or as a path.
#[test]
fn a_name_found_nowhere_in_the_repository_is_still_rejected() {
    let files = repo_files();
    let repo = RepoNames::new(&files);
    for (fix, bad) in [
        (
            "Add the constraint `billing_accounts_user_id_fkey` to the table.",
            "billing_accounts_user_id_fkey",
        ),
        (
            "Create `app/api/charge/route.ts` and move the handler there.",
            "app/api/charge/route.ts",
        ),
        (
            "Call `rotateServiceKey` before the write.",
            "rotateServiceKey",
        ),
        (
            "Use `const key = rotateServiceKey(actor_id)` in the handler.",
            "rotateServiceKey",
        ),
        ("Also update lib/guard/index.ts.", "lib/guard/index.ts"),
    ] {
        assert_eq!(
            find_ungrounded_identifier_in(fix, WINDOW, Some(&repo)).as_deref(),
            Some(bad),
            "{fix}"
        );
    }
}

#[test]
fn the_repository_lookup_matches_paths_directories_and_words() {
    let files = repo_files();
    let repo = RepoNames::new(&files);
    assert!(repo.knows("lib/audit.ts"));
    assert!(repo.knows("audit.ts"));
    assert!(repo.knows("lib"));
    assert!(repo.knows("supabase/migrations"));
    assert!(repo.knows("./tsconfig.json"));
    assert!(repo.knows("actor_id"));
    assert!(repo.knows("public.billing_accounts"));
    assert!(!repo.knows("lib/other.ts"));
    assert!(!repo.knows("actor_ids_all"));
    assert!(!repo.knows(""));
}

/// Answers every fix prompt with a fix that names `name` in backticks, for the first finding of
/// the prompt, and keeps the prompts it was sent.
struct NamingLlm {
    name: &'static str,
    prompts: std::sync::Mutex<Vec<String>>,
}

impl NamingLlm {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl crate::llm::LlmPort for NamingLlm {
    async fn complete(
        &self,
        req: crate::llm::LlmRequest,
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        let ids = super::reply_schemas::check::prompt_ids(&req.prompt);
        let text = serde_json::json!({"fixes": [{
            "id": ids[0],
            "fix": format!("Record `{}` before the write.", self.name)
        }]})
        .to_string();
        self.prompts.lock().unwrap().push(req.prompt);
        Ok(super::structured_output_tests::response(
            &text,
            "api",
            "claude-sonnet-5-5",
        ))
    }
    async fn complete_streaming(
        &self,
        req: crate::llm::LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Through the stage: a fix naming a repository name outside its window is accepted in the first
/// round, with no regeneration; one naming an invented name is turned down and the model is told
/// the name is nowhere in the repository.
#[tokio::test]
async fn the_fix_stage_accepts_a_repository_name_and_turns_down_an_invented_one() {
    let f = finding(
        "src/work.ts",
        2,
        "`runWork` passes `raw` to `doWork`",
        "doWork(raw)",
    );
    let llm = NamingLlm::new("actor_id");
    let (out, failed) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![f.clone()],
        &repo_files(),
        None,
        None,
        None,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(out[0].fix_specific.as_deref().unwrap().contains("actor_id"));
    assert_eq!(llm.prompts().len(), 1, "accepted in the first round");

    let llm = NamingLlm::new("billing_accounts_user_id_fkey");
    let (out, failed) =
        generate_fix_specifics(&llm, "o/r", vec![f], &repo_files(), None, None, None).await;
    assert_eq!(failed.len(), 1, "an invented name is a give-up");
    assert!(out[0].fix_specific.is_none());
    assert!(
        llm.prompts()
            .iter()
            .any(|p| p.contains("billing_accounts_user_id_fkey")
                && p.contains("nor anywhere in the repository")),
        "the retry tells the model the name is nowhere in the repository"
    );
    let reasons = failed[0]
        .attempts
        .iter()
        .flat_map(|a| a.rejections.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        reasons.contains("billing_accounts_user_id_fkey")
            && reasons.contains("anywhere in the repository"),
        "{reasons}"
    );
}
