//! The draft-to-publish gate.
//!
//! Publishing an app is a one-way door: a live URL is real and may already
//! have been visited by the user or shared with others. The gate enforces two
//! pre-conditions before the deploy target is called:
//!
//! 1. **At least one successful build.** The app must have been built and
//!    passed QA at least once. Publishing an unbuilt project is always wrong.
//! 2. **Explicit user confirmation.** The user must have clicked "Publish" (or
//!    the equivalent) in the UI. Nothing auto-publishes. This is the
//!    draft-to-live decision the user owns.
//!
//! Use [`can_publish`] for a lightweight guard, or [`PublishGate`] to carry
//! the confirmation state as a typed value through a workflow.

use thiserror::Error;

/// Reasons the draft-to-publish transition is not allowed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PublishError {
    /// The app has not completed a successful build yet. The user must build
    /// (and optionally QA) before publishing.
    #[error("the app has not been built successfully; complete a build before publishing")]
    NotYetBuilt,
    /// The user has not explicitly confirmed the publish action. Publish is
    /// always a deliberate choice; it is never automatic.
    #[error("publish requires explicit user confirmation; the user has not confirmed yet")]
    NotConfirmed,
}

/// Guard: check whether the publish pre-conditions are met.
///
/// `executed` must be `true` when at least one successful build has
/// completed. `user_confirmed` must be `true` when the user has explicitly
/// clicked "Publish" in the UI (or the equivalent action in the CLI).
///
/// Returns `Ok(())` when both pre-conditions hold. Returns the FIRST
/// failing [`PublishError`] otherwise (build check takes priority).
///
/// # Examples
///
/// ```
/// use camerata_deploy::gate::{can_publish, PublishError};
///
/// assert!(can_publish(true, true).is_ok());
/// assert_eq!(can_publish(false, true), Err(PublishError::NotYetBuilt));
/// assert_eq!(can_publish(true, false), Err(PublishError::NotConfirmed));
/// assert_eq!(can_publish(false, false), Err(PublishError::NotYetBuilt));
/// ```
pub fn can_publish(executed: bool, user_confirmed: bool) -> Result<(), PublishError> {
    if !executed {
        return Err(PublishError::NotYetBuilt);
    }
    if !user_confirmed {
        return Err(PublishError::NotConfirmed);
    }
    Ok(())
}

// ─── typed state machine (optional, richer workflow path) ──────────────────

/// A project whose app has not yet been published. Carries proof that the
/// gate pre-conditions are tracked through the type system.
///
/// Call [`PublishGate::publish`] to attempt the `Draft -> Published`
/// transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishGate {
    /// Whether at least one successful build has been completed.
    pub build_executed: bool,
    /// Whether the user has explicitly confirmed the publish action.
    pub user_confirmed: bool,
}

/// A project whose draft-to-live transition has been approved by the gate.
///
/// Receiving a `Published` value is proof that both [`PublishGate`]
/// pre-conditions were satisfied at the moment of transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published;

impl PublishGate {
    /// Construct a gate with explicit pre-condition state.
    pub fn new(build_executed: bool, user_confirmed: bool) -> Self {
        Self {
            build_executed,
            user_confirmed,
        }
    }

    /// Attempt the draft-to-live transition.
    ///
    /// Returns `Ok(Published)` when both pre-conditions hold. Returns the
    /// first failing [`PublishError`] otherwise. Consumes `self` so the gate
    /// cannot be re-used after a successful transition.
    pub fn publish(self) -> Result<Published, PublishError> {
        can_publish(self.build_executed, self.user_confirmed)?;
        Ok(Published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- can_publish guard ---

    #[test]
    fn can_publish_ok_when_both_conditions_met() {
        assert!(can_publish(true, true).is_ok());
    }

    #[test]
    fn can_publish_blocks_without_build() {
        assert_eq!(can_publish(false, true), Err(PublishError::NotYetBuilt));
    }

    #[test]
    fn can_publish_blocks_without_confirmation() {
        assert_eq!(can_publish(true, false), Err(PublishError::NotConfirmed));
    }

    #[test]
    fn can_publish_blocks_when_neither_condition_met() {
        // Build check takes priority.
        assert_eq!(can_publish(false, false), Err(PublishError::NotYetBuilt));
    }

    // --- PublishGate state machine ---

    #[test]
    fn gate_publish_succeeds_when_ready() {
        let gate = PublishGate::new(true, true);
        assert!(gate.publish().is_ok());
    }

    #[test]
    fn gate_publish_fails_not_yet_built() {
        let gate = PublishGate::new(false, true);
        assert_eq!(gate.publish(), Err(PublishError::NotYetBuilt));
    }

    #[test]
    fn gate_publish_fails_not_confirmed() {
        let gate = PublishGate::new(true, false);
        assert_eq!(gate.publish(), Err(PublishError::NotConfirmed));
    }

    #[test]
    fn gate_publish_fails_neither_condition() {
        let gate = PublishGate::new(false, false);
        assert_eq!(gate.publish(), Err(PublishError::NotYetBuilt));
    }

    #[test]
    fn publish_error_display_is_readable() {
        assert!(!PublishError::NotYetBuilt.to_string().is_empty());
        assert!(!PublishError::NotConfirmed.to_string().is_empty());
    }
}
