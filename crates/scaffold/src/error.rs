use std::path::PathBuf;

/// Errors `scaffold_skeleton` can return. Deliberately narrow: template
/// materialization is pure string substitution plus file writes, so the only real
/// failure mode is I/O.
#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write scaffold file {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
