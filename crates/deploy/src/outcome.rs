//! Deploy outcome types: status, result, and convenience constructors.

use serde::{Deserialize, Serialize};

/// The lifecycle phase of a deploy operation.
///
/// Variants are serialized in `snake_case` for JSON/storage compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployStatus {
    /// The deploy has been enqueued but has not started yet.
    Pending,
    /// The deploy is actively in progress.
    Deploying,
    /// The deploy completed successfully and the app is reachable.
    Live,
    /// The deploy failed. See [`DeployOutcome::message`] for the reason.
    Failed,
}

/// The full result of a deploy attempt.
///
/// Returned by [`DeployTarget::deploy`][crate::target::DeployTarget::deploy].
/// Inspect [`DeployOutcome::is_live`] for a quick boolean check; read
/// [`DeployOutcome::url`] for the live URL when the deploy succeeded, and
/// [`DeployOutcome::log`] for the ordered step-by-step output of what the
/// target did (or planned to do).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployOutcome {
    /// Whether the deploy succeeded, is in-flight, is pending, or has failed.
    pub status: DeployStatus,
    /// The public URL of the deployed application. `None` when the app is not
    /// yet live.
    pub url: Option<String>,
    /// Ordered list of log lines produced during the deploy (steps taken,
    /// commands run, or the deploy plan for targets that emit one).
    pub log: Vec<String>,
    /// A human-readable message. Present when the deploy failed or when a
    /// target needs to explain why full execution was not attempted.
    pub message: Option<String>,
}

impl DeployOutcome {
    /// `true` only when the deploy has fully completed and the app is
    /// reachable at [`DeployOutcome::url`].
    pub fn is_live(&self) -> bool {
        self.status == DeployStatus::Live
    }

    /// Construct a successful `Live` outcome with the given public URL.
    ///
    /// `log` is initially empty; add entries via the returned struct if needed.
    pub fn live(url: impl Into<String>) -> Self {
        Self {
            status: DeployStatus::Live,
            url: Some(url.into()),
            log: Vec::new(),
            message: None,
        }
    }

    /// Construct a `Failed` outcome with a human-readable message explaining
    /// what went wrong.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            status: DeployStatus::Failed,
            url: None,
            log: Vec::new(),
            message: Some(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_live_true_for_live_status() {
        let outcome = DeployOutcome::live("http://localhost:8080/my-app");
        assert!(outcome.is_live());
        assert_eq!(outcome.url.as_deref(), Some("http://localhost:8080/my-app"));
        assert!(outcome.log.is_empty());
        assert!(outcome.message.is_none());
    }

    #[test]
    fn is_live_false_for_non_live_statuses() {
        for status in [
            DeployStatus::Pending,
            DeployStatus::Deploying,
            DeployStatus::Failed,
        ] {
            let outcome = DeployOutcome {
                status,
                url: None,
                log: Vec::new(),
                message: None,
            };
            assert!(!outcome.is_live(), "{status:?} must not be live");
        }
    }

    #[test]
    fn failed_constructor_sets_fields() {
        let outcome = DeployOutcome::failed("credentials missing");
        assert_eq!(outcome.status, DeployStatus::Failed);
        assert!(outcome.url.is_none());
        assert_eq!(outcome.message.as_deref(), Some("credentials missing"));
    }

    #[test]
    fn status_round_trip_json_snake_case() {
        for (status, expected) in [
            (DeployStatus::Pending, "\"pending\""),
            (DeployStatus::Deploying, "\"deploying\""),
            (DeployStatus::Live, "\"live\""),
            (DeployStatus::Failed, "\"failed\""),
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(
                json, expected,
                "status {status:?} must serialize to {expected}"
            );
            let back: DeployStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn outcome_round_trip_json() {
        let outcome = DeployOutcome {
            status: DeployStatus::Live,
            url: Some("https://my-app.azurewebsites.net".to_string()),
            log: vec!["step 1".to_string(), "step 2".to_string()],
            message: None,
        };
        let json = serde_json::to_string(&outcome).unwrap();
        let back: DeployOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, outcome);
    }
}
