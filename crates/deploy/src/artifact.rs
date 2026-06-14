//! The build artifact handed to a [`DeployTarget`][crate::target::DeployTarget].
//!
//! A `DeployArtifact` is produced by the build step and consumed by the
//! publish step. It carries just enough information for the deploy target to
//! locate the built output and give the deployed app a name.

use serde::{Deserialize, Serialize};

/// The built output ready to be handed off to a deploy target.
///
/// `app_name` is the human-readable project name (e.g. "Pottery Studio Admin").
/// `build_dir` is the local filesystem path (as a `String`) to the directory
/// that contains the compiled, ready-to-ship application.
///
/// Both fields are plain strings so the struct stays serde-compatible and
/// dependency-free; callers that have a `std::path::PathBuf` should call
/// `.to_string_lossy().into_owned()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployArtifact {
    /// The human-readable name of the application being deployed.
    pub app_name: String,
    /// Filesystem path to the build output directory, as a plain string.
    pub build_dir: String,
}

impl DeployArtifact {
    /// Construct a deploy artifact.
    pub fn new(app_name: impl Into<String>, build_dir: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
            build_dir: build_dir.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_json() {
        let artifact = DeployArtifact::new("Pottery Studio", "/tmp/pottery-studio/dist");
        let json = serde_json::to_string(&artifact).unwrap();
        let back: DeployArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(back, artifact);
    }

    #[test]
    fn new_stores_fields() {
        let a = DeployArtifact::new("My App", "/some/dir");
        assert_eq!(a.app_name, "My App");
        assert_eq!(a.build_dir, "/some/dir");
    }
}
