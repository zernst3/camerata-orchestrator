//! Minimal library surface for `camerata-ui`.
//!
//! The cockpit is a BINARY (`src/main.rs`); this lib target exists so that the
//! runtime clipboard probe (`examples/clipboard_probe.rs`) can exercise the
//! exact desktop-chrome code (menu bar, clipboard shim, key handling) that the
//! shipping binary installs.  Do not grow this into a general-purpose library:
//! app modules stay private to the binary.

pub mod clipboard_probe;
pub mod desktop_chrome;
