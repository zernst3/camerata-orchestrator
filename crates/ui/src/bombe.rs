//! `BombeSpinner` — a tiny homage to the Bombe (the Bletchley Park codebreaking
//! machine Turing's team ran). A 4×2 grid of rotor "drums," each row turning at a
//! different speed like an odometer: the top row spins fast, the row below it slower.
//! Used as an "it's actually working" affordance during long AI steps.
//!
//! Reusable and self-contained: drop `BombeSpinner {}` anywhere. The per-drum
//! animation timing is computed inline (duration by row, phase offset by column) so
//! the whole thing is one small component plus a single keyframe in `style.rs`.

use dioxus::prelude::*;

/// Number of drums per row and number of rows — a 4×2 grid, like a small Bombe bank.
const COLS: usize = 4;
const ROWS: usize = 2;

/// The Bombe rotor-bank spinner. Optional `title` sets the hover tooltip.
#[component]
pub fn BombeSpinner(#[props(default)] title: Option<String>) -> Element {
    let tip = title.unwrap_or_else(|| "working\u{2026}".to_string());
    rsx! {
        div { class: "bombe", title: "{tip}", role: "status", "aria-label": "{tip}",
            for row in 0..ROWS {
                div { class: "bombe-row", key: "{row}",
                    for col in 0..COLS {
                        {
                            // Top row turns fastest; each row below is markedly slower
                            // (odometer cascade). Columns are phase-offset so the marks
                            // don't all line up — that's what reads as "machine running".
                            let dur = 0.7 + (row as f64) * 1.25;
                            let delay = -(col as f64) * (dur / COLS as f64);
                            rsx! {
                                div { class: "bombe-drum", key: "{col}",
                                    div {
                                        class: "bombe-mark",
                                        style: "animation-duration: {dur}s; animation-delay: {delay}s;",
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
