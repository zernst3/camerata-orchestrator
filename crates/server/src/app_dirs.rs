//! Where Camerata keeps its per-user stores, with one seam for tests and sandboxes.
//!
//! The one implementation lives in `camerata_api_types::data_dir` (#566), so the app can work out
//! the folder a server it starts or attaches to should report; it is re-exported here so every
//! `crate::app_dirs::X` call site is unchanged. See that module for the resolution rules.

pub use camerata_api_types::data_dir::{camerata_dir, data_dir, DATA_DIR_ENV};
