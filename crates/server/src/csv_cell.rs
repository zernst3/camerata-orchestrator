//! CSV injection (V1 item 33, finding 39): the one neutralizing function lives in
//! `camerata_ui_core::csv_cell` (the cockpit's CSV downloads use it too); every package writer
//! and the release gate reach it through here.

pub use camerata_ui_core::csv_cell::{neutralize, safe_record, starts_a_formula};
