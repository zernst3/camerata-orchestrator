//! `BombeBg` — the full Bletchley Park Bombe machine as a fixed, full-viewport
//! background layer rendered BEHIND the application shell.
//!
//! ## Architecture
//!
//! The cabinet is fixed-position, `inset: 0`, `z-index: 0`, `pointer-events: none`.
//! The app shell sits on top at `z-index: 1` (or higher).
//!
//! ## Rotor optimisation
//!
//! The original mockup's JS generated ~108 rotors × ~7 nested elements ≈ 756 DOM
//! nodes.  Here each rotor is a *single* `<div class="bg-bombe-rotor">` using
//! layered `background` and CSS `::before` / `::after` pseudo-elements to paint
//! the outer contacts ring, bakelite disc, and centre hub — all without extra
//! child nodes.  The spinning drum `<div class="rotor-drum">` is ONE child.  So
//! the real DOM cost per rotor is **2 elements** (socket + drum), totalling ~216
//! nodes instead of ~756.
//!
//! ## Tick math
//!
//! Each revolution has 26 steps (`steps(26, end)`), matching the 26 Enigma
//! alphabet positions.  The per-row durations (0.9 s / 26 s / 78 s) are the
//! mockup's values; they give ticks at ≈0.035 s / 1 s / 3 s respectively.
//!
//! ## Running state
//!
//! When the loading count > 0 the parent adds `.bombe-running` to the
//! `#bg-bombe-machine` element.  All animations are gated on `.bombe-running` so
//! idle cost is zero.

use dioxus::prelude::*;
use crate::loading::{BombeEnabled, BombePreview, LoadingCount};

/// Total rotors = 3 blocks × 12 columns × 3 rows = 108.
const BLOCKS: usize = 3;
const ROTORS_PER_BLOCK: usize = 36;

/// Pre-computed start-angle offsets (degrees) for the 36 rotors inside each
/// block.  A deterministic pseudo-random spread keeps the cabinet looking
/// lively without requiring JS `Math.random()`.  Values cycle through a small
/// set — visually varied enough, no alloc.
const START_ANGLES: [u16; 12] = [0, 137, 274, 51, 188, 325, 102, 239, 16, 153, 290, 67];

/// Background Bombe machine.
///
/// Renders as a fixed, full-viewport layer behind the app (z-index 0,
/// pointer-events none).  Adds `.bombe-running` when the effective running
/// state is true:  `running = enabled && (count > 0 || preview)`.
///
/// Also renders `.bombe-overlay` — a sibling fixed layer at z-index 2 that
/// sits between the bombe (z-index 0) and the app shell (z-index 10+).
/// Idle: strong dark fill so the bombe is visible-but-subtle.
/// Running: the overlay gets `.bombe-overlay-running`, which lowers its
/// opacity so the bombe glows through more clearly.  `pointer-events: none`
/// on both layers so the app is never blocked.
#[component]
pub fn BombeBg() -> Element {
    // Read the loading count + control signals.  All three use try_consume so
    // BombeBg is safe even if mounted before the context is provided.
    let count = match try_consume_context::<LoadingCount>() {
        Some(c) => *c.read(),
        None => 0,
    };
    let enabled = match try_consume_context::<BombeEnabled>() {
        Some(s) => *s.0.read(),
        None => true,
    };
    let preview = match try_consume_context::<BombePreview>() {
        Some(s) => *s.0.read(),
        None => false,
    };

    // The effective running state (shared, unit-tested formula): animations only fire when the
    // bombe is enabled AND either real AI work is in-flight OR the preview is active. The count is
    // driven solely by LoadingGuards around AI/heavy work, so the Bombe stays reserved for it.
    let running = crate::loading::bombe_running(enabled, count, preview);

    let machine_class = if running {
        "bombe-bg-machine bombe-running"
    } else {
        "bombe-bg-machine"
    };
    let overlay_class = if running {
        "bombe-overlay bombe-overlay-running"
    } else {
        "bombe-overlay"
    };

    rsx! {
        // Dark obscuring overlay — sits BETWEEN the bombe (z-index 0) and the
        // app shell (z-index 10+) at z-index 2.  pointer-events:none so it
        // never intercepts clicks.  Lightens when the bombe is running so the
        // machine glows through while text stays readable.
        div { class: "{overlay_class}" }

        div { id: "bg-bombe-machine", class: "{machine_class}",
            div { class: "bombe-cabinet",
                // ── Left panel: two gauges + vertical cable loom ──────────────
                div { class: "bombe-panel left-control-panel",
                    div { class: "bombe-gauge", div { class: "bombe-needle" } }
                    div { class: "bombe-gauge", div { class: "bombe-needle" } }
                    div { class: "bombe-cable-bundle" }
                }

                // ── Centre matrix: 3 blocks × 36 rotors ───────────────────────
                div { class: "bombe-rotors-matrix",
                    for b in 0..BLOCKS {
                        div { class: "bombe-block", key: "{b}",
                            for r in 0..ROTORS_PER_BLOCK {
                                {
                                    let row_in_block = r / 12;  // 0 | 1 | 2
                                    let col = r % 12;
                                    let angle = START_ANGLES[col];
                                    // Row duration matching the mockup's tick cadence.
                                    let dur = match row_in_block {
                                        0 => "0.9s",
                                        1 => "26.0s",
                                        _ => "78.0s",
                                    };
                                    rsx! {
                                        BgRotor {
                                            key: "{b}-{r}",
                                            row: row_in_block,
                                            start_angle: angle,
                                            duration: dur,
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Right panel: OUTPUT label + status LEDs + cable loom ──────
                div { class: "bombe-panel right-control-panel",
                    span { class: "bombe-panel-label", "OUTPUT" }
                    div { class: "bombe-status-leds",
                        div { class: "bombe-led-bulb active" }
                        div { class: "bombe-led-bulb" }
                        div { class: "bombe-led-bulb" }
                    }
                    div { class: "bombe-cable-bundle right-cables" }
                }
            }
        }
    }
}

/// One rotor cell — a single socket div containing a spinning drum div.
///
/// All visual detail (outer contacts ring, bakelite disc, centre hub, pointer)
/// is painted by CSS backgrounds and pseudo-elements on these two elements,
/// so the DOM is exactly 2 nodes per rotor (108 rotors × 2 = 216 total).
///
/// Props:
/// - `row`: 0 = top (fast), 1 = middle, 2 = bottom (slow)
/// - `start_angle`: initial rotation offset in degrees (for visual spread)
/// - `duration`: CSS animation-duration string (e.g. `"0.9s"`)
#[component]
fn BgRotor(row: usize, start_angle: u16, duration: &'static str) -> Element {
    // Row-specific classes drive the bakelite-disc colour and pointer colour
    // via CSS nth-child selectors — but since we control the class name
    // directly we can also add an explicit row class that is more readable.
    let row_class = match row {
        0 => "bg-bombe-rotor bombe-row-top",
        1 => "bg-bombe-rotor bombe-row-mid",
        _ => "bg-bombe-rotor bombe-row-bot",
    };
    rsx! {
        div { class: "{row_class}",
            div {
                class: "rotor-drum",
                style: "--start-angle:{start_angle}deg; animation-duration:{duration};",
            }
        }
    }
}

#[cfg(test)]
mod render_tests {
    use super::{BgRotor, BombeBg};
    use crate::loading::{BombeEnabled, BombePreview, LoadingCount};
    use dioxus::prelude::*;

    // Tier-1 render test: mount the component inside a VirtualDom (so its hooks /
    // context reads run), render to a static HTML string with dioxus-ssr, and assert
    // the STRUCTURE. No browser / wasm, no interaction or async-loaded data.

    // ── BombeBg ──────────────────────────────────────────────────────────────
    //
    // BombeBg reads its three control signals via try_consume_context, so it
    // renders safely with NO context provided (defaults: enabled=true, count=0,
    // preview=false → running=false → idle classes).

    fn bombe_idle_harness() -> Element {
        rsx! { BombeBg {} }
    }

    #[test]
    fn bombe_bg_renders_core_structure() {
        let mut vdom = VirtualDom::new(bombe_idle_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        // The machine root, the obscuring overlay, the cabinet and its panels.
        assert!(
            html.contains(r#"id="bg-bombe-machine""#),
            "the machine root renders; html=\n{html}"
        );
        assert!(
            html.contains("bombe-overlay"),
            "the obscuring overlay renders; html=\n{html}"
        );
        assert!(
            html.contains("bombe-cabinet"),
            "the cabinet renders; html=\n{html}"
        );
        assert!(
            html.contains("left-control-panel") && html.contains("right-control-panel"),
            "both control panels render; html=\n{html}"
        );
        // The right panel's OUTPUT label + the rotor matrix.
        assert!(
            html.contains("OUTPUT"),
            "the OUTPUT label renders; html=\n{html}"
        );
        assert!(
            html.contains("bombe-rotors-matrix"),
            "the rotor matrix renders; html=\n{html}"
        );
    }

    #[test]
    fn bombe_bg_idle_state_omits_running_classes() {
        // No context → defaults → running=false. The machine and overlay must NOT
        // carry their *-running modifier classes.
        let mut vdom = VirtualDom::new(bombe_idle_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(
            html.contains("bombe-bg-machine"),
            "the base machine class is present; html=\n{html}"
        );
        assert!(
            !html.contains("bombe-running"),
            "idle: no bombe-running class; html=\n{html}"
        );
        assert!(
            !html.contains("bombe-overlay-running"),
            "idle: no bombe-overlay-running class; html=\n{html}"
        );
    }

    // Provide the three control contexts with preview=true so running=true
    // (enabled=true && (count==0 → false || preview==true → true)).
    fn bombe_running_harness() -> Element {
        use_context_provider(|| Signal::new(0_usize) as LoadingCount);
        use_context_provider(|| BombeEnabled(Signal::new(true)));
        use_context_provider(|| BombePreview(Signal::new(true)));
        rsx! { BombeBg {} }
    }

    #[test]
    fn bombe_bg_running_state_adds_running_classes() {
        let mut vdom = VirtualDom::new(bombe_running_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(
            html.contains("bombe-running"),
            "running: the machine gets bombe-running; html=\n{html}"
        );
        assert!(
            html.contains("bombe-overlay-running"),
            "running: the overlay gets bombe-overlay-running; html=\n{html}"
        );
    }

    #[test]
    fn bombe_bg_disabled_stays_idle_even_with_preview() {
        // enabled=false short-circuits running to false regardless of preview/count.
        fn harness() -> Element {
            use_context_provider(|| Signal::new(5_usize) as LoadingCount);
            use_context_provider(|| BombeEnabled(Signal::new(false)));
            use_context_provider(|| BombePreview(Signal::new(true)));
            rsx! { BombeBg {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(
            !html.contains("bombe-running"),
            "disabled: no bombe-running even with preview/count; html=\n{html}"
        );
        assert!(
            !html.contains("bombe-overlay-running"),
            "disabled: no bombe-overlay-running; html=\n{html}"
        );
    }

    // ── BgRotor ──────────────────────────────────────────────────────────────
    //
    // Prop-only component (no context, no hooks). The row index selects the
    // CSS row class; start_angle + duration flow into the drum's inline style.

    #[test]
    fn bg_rotor_top_row_renders_class_and_style() {
        fn harness() -> Element {
            rsx! {
                BgRotor { row: 0_usize, start_angle: 137_u16, duration: "0.9s" }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        assert!(
            html.contains("bg-bombe-rotor") && html.contains("bombe-row-top"),
            "row 0 → top row class; html=\n{html}"
        );
        assert!(
            html.contains("rotor-drum"),
            "the spinning drum child renders; html=\n{html}"
        );
        assert!(
            html.contains("--start-angle:137deg"),
            "start_angle flows into the inline style; html=\n{html}"
        );
        assert!(
            html.contains("animation-duration:0.9s"),
            "duration flows into the inline style; html=\n{html}"
        );
    }

    #[test]
    fn bg_rotor_mid_and_bot_rows_select_distinct_classes() {
        fn mid_harness() -> Element {
            rsx! { BgRotor { row: 1_usize, start_angle: 0_u16, duration: "26.0s" } }
        }
        fn bot_harness() -> Element {
            rsx! { BgRotor { row: 2_usize, start_angle: 0_u16, duration: "78.0s" } }
        }

        let mut mid = VirtualDom::new(mid_harness);
        mid.rebuild_in_place();
        let mid_html = dioxus_ssr::render(&mid);
        assert!(
            mid_html.contains("bombe-row-mid"),
            "row 1 → mid row class; html=\n{mid_html}"
        );

        let mut bot = VirtualDom::new(bot_harness);
        bot.rebuild_in_place();
        let bot_html = dioxus_ssr::render(&bot);
        assert!(
            bot_html.contains("bombe-row-bot"),
            "row 2 (and beyond) → bot row class; html=\n{bot_html}"
        );
    }
}
