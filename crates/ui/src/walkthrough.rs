//! The inspection walkthrough, the Dioxus side (V1 item 25;
//! `docs/design/2026-10-27_inspection-walkthrough.md`).
//!
//! Every rule is in `camerata_ui_core::walkthrough`: the step table, which step is current, what
//! moves it, when the popup yields, where it sits, what is remembered. This file owns what needs
//! a window:
//!
//! * [`TourHandle`] / [`provide_tour`]: the walkthrough's state, shared through context. The
//!   handle's [`TourHandle::start`] is THE one function that starts it; the Help menu
//!   ([`HelpMenuListener`]), the command palette's "Start walkthrough" line, the "Guided
//!   inspection" switch ([`GuidedSwitch`]) and the first-launch offer ([`WalkthroughOffer`]) all
//!   call it.
//! * [`WalkthroughHost`]: looks at the page a few times a second (which `data-tour` controls are
//!   on screen, where the current one is, which was clicked, whether a reason prompt, an overlay
//!   or a dialog is up) and draws the popup and the highlight.
//!
//! It never clicks, types or decides anything for the user. The only thing it does to the page
//! besides drawing is scroll the current control into view once per step, and move the keyboard
//! (into the popup when the walkthrough is started, back to the highlighted control when it is
//! closed with Exit, Done or Esc).
//!
//! Remembered per user in the webview's own storage, the same store as the background animation
//! switch, and never in a project (so no project export carries it).

use std::collections::BTreeSet;
use std::time::Duration;

use camerata_ui_core::walkthrough::{
    guided_word, parse_prefs, place, popup_mode, should_offer, tour_gate, Offer, PopupMode,
    Progress, Rect, Side, Tour, TourGate, TourPrefs, Viewport, KEY_GUIDED, KEY_OFFER,
    TOUR_ATTRIBUTE,
};
use dioxus::prelude::*;

/// How often the host looks at the page while the walkthrough is running.
const PROBE_MS: u64 = 300;

/// The DOM id of the popup, for the keyboard.
pub(crate) const POPUP_ID: &str = "camerata-tour-popup";

// ── The page, as JavaScript ──────────────────────────────────────────────────────────────

/// What owns the screen, as selectors. A reason prompt is the pane's or the toolbar's (each is in
/// the page only while open). An overlay is the code viewer, the stacked view (both `cv-overlay`)
/// or the command palette. A dialog is any other `.rule-modal-overlay`. The page is asked, not
/// this code's own state, so a dialog nobody listed is still seen: the review bridge reads the
/// page the same way for the palette's chord.
const REASON_SELECTOR: &str = ".queue-reason-prompt, .pane-reason-prompt";
const OVERLAY_SELECTOR: &str = ".cv-overlay, .pal-overlay";
const MODAL_SELECTOR: &str = ".rule-modal-overlay:not(.cv-overlay):not(.pal-overlay)";

/// Install the page-side listeners, once: a click anywhere is noted by the nearest `data-tour`
/// control it landed in, and Esc is noted when nothing else owns it (no reason prompt, overlay or
/// dialog is up, and the keyboard is not in a field). Neither listener stops, prevents or
/// consumes an event.
fn install_script() -> String {
    format!(
        r#"if (!window.__camerataTourInstalled) {{
  window.__camerataTourInstalled = true;
  window.__camerataTourClicks = [];
  window.__camerataTourEsc = false;
  document.addEventListener("click", function (e) {{
    var t = e.target && e.target.closest ? e.target.closest("[{TOUR_ATTRIBUTE}]") : null;
    if (t) window.__camerataTourClicks.push(t.getAttribute("{TOUR_ATTRIBUTE}"));
  }}, true);
  document.addEventListener("keydown", function (e) {{
    if (e.key !== "Escape" || e.ctrlKey || e.metaKey || e.altKey) return;
    if (document.querySelector("{REASON_SELECTOR}, {OVERLAY_SELECTOR}, {MODAL_SELECTOR}")) return;
    var a = document.activeElement;
    var tag = a && a.tagName ? a.tagName.toLowerCase() : "";
    if (tag === "input" || tag === "textarea" || tag === "select" || (a && a.isContentEditable)) return;
    window.__camerataTourEsc = true;
  }}, true);
}}"#
    )
}

/// One look at the page for the step whose control is `target`. Answers (to `dioxus.send`) the
/// `data-tour` ids on screen, the control's rectangle, the window size, what owns the screen, the
/// ids clicked since the last look, and whether Esc was pressed. When `scroll` is set it first
/// scrolls the control into view if it is outside the window.
pub(crate) fn probe_js(target: &str, scroll: bool) -> String {
    let target_json = serde_json::to_string(target).unwrap_or_else(|_| "\"\"".to_string());
    let install = install_script();
    format!(
        r#"{install}
var target = {target_json};
var visible = function (el) {{
  var r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return false;
  var s = window.getComputedStyle(el);
  return s.visibility !== "hidden" && s.display !== "none";
}};
var ids = [];
var found = null;
document.querySelectorAll("[{TOUR_ATTRIBUTE}]").forEach(function (el) {{
  if (!visible(el)) return;
  var id = el.getAttribute("{TOUR_ATTRIBUTE}");
  if (ids.indexOf(id) < 0) ids.push(id);
  if (id === target && !found) found = el;
}});
var rect = null;
if (found) {{
  var r = found.getBoundingClientRect();
  var outside = r.top < 0 || r.bottom > window.innerHeight || r.left < 0 || r.right > window.innerWidth;
  if ({scroll} && outside) {{
    found.scrollIntoView({{ block: "center", inline: "nearest" }});
    r = found.getBoundingClientRect();
  }}
  rect = {{ x: r.left, y: r.top, w: r.width, h: r.height }};
}}
var clicks = window.__camerataTourClicks; window.__camerataTourClicks = [];
var esc = window.__camerataTourEsc; window.__camerataTourEsc = false;
dioxus.send({{
  target: target,
  ids: ids,
  rect: rect,
  vw: window.innerWidth,
  vh: window.innerHeight,
  reason: !!document.querySelector("{REASON_SELECTOR}"),
  overlay: !!document.querySelector("{OVERLAY_SELECTOR}"),
  modal: !!document.querySelector("{MODAL_SELECTOR}"),
  clicks: clicks,
  esc: esc
}});"#
    )
}

/// What owns the screen right now, asked of the page: used to refuse starting the walkthrough
/// under a reason prompt, an overlay or a dialog. The palette's own overlay is not counted (the
/// palette is open when its line is run, and it closes itself).
fn gate_js() -> String {
    format!(
        r#"dioxus.send({{
  reason: !!document.querySelector("{REASON_SELECTOR}"),
  overlay: !!document.querySelector(".cv-overlay"),
  modal: !!document.querySelector("{MODAL_SELECTOR}")
}});"#
    )
}

/// Read the two remembered words.
fn read_prefs_js() -> String {
    format!(
        r#"var g = null, o = null;
try {{ g = localStorage.getItem("{KEY_GUIDED}"); o = localStorage.getItem("{KEY_OFFER}"); }} catch (e) {{}}
dioxus.send({{ guided: g, offer: o }});"#
    )
}

/// Write one remembered word.
fn write_pref_js(key: &str, value: &str) -> String {
    let key = serde_json::to_string(key).unwrap_or_default();
    let value = serde_json::to_string(value).unwrap_or_default();
    format!("try {{ localStorage.setItem({key}, {value}); }} catch (e) {{}}")
}

/// Put the keyboard on the popup.
fn focus_popup_js() -> String {
    format!("var p = document.getElementById(\"{POPUP_ID}\"); if (p) p.focus();")
}

/// Put the keyboard back on the control the popup was pointing at: the control itself when it can
/// take focus, else the first thing inside it that can. Nothing when it is gone.
pub(crate) fn focus_control_js(target: &str) -> String {
    let target = serde_json::to_string(target).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"var el = document.querySelector('[{TOUR_ATTRIBUTE}=' + JSON.stringify({target}) + ']');
if (el) {{
  var focusable = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';
  var to = el.matches(focusable) ? el : el.querySelector(focusable);
  if (to && typeof to.focus === 'function') to.focus();
}}"#
    )
}

// ── What the page said ───────────────────────────────────────────────────────────────────

/// One look at the page, parsed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Probe {
    /// The `data-tour` id the look was for.
    pub target: String,
    /// Every `data-tour` id on screen.
    pub ids: BTreeSet<String>,
    /// Where the target is, if it is on screen.
    pub rect: Option<Rect>,
    pub viewport: Viewport,
    pub gate: TourGate,
    /// The `data-tour` ids clicked since the last look.
    pub clicks: Vec<String>,
    /// Esc was pressed with nothing else owning it.
    pub esc: bool,
}

/// Hand one look at the page to the tour: what the user did first (a click can also be what
/// opens a dialog), then what the screen now shows. Answers whether the tour moved on.
pub(crate) fn apply_probe(tour: &mut Tour, probe: &Probe) -> bool {
    let mut moved = false;
    for id in &probe.clicks {
        moved |= tour.click(id);
    }
    moved | tour.observe(&probe.ids)
}

/// Read a probe from what the page sent. `None` when it is not the shape asked for.
pub(crate) fn parse_probe(v: &serde_json::Value) -> Option<Probe> {
    let flag = |k: &str| v.get(k).and_then(serde_json::Value::as_bool);
    let num = |o: &serde_json::Value, k: &str| o.get(k).and_then(serde_json::Value::as_f64);
    let strings = |k: &str| -> Vec<String> {
        v.get(k)
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let rect = match v.get("rect") {
        Some(o) if o.is_object() => Some(Rect {
            x: num(o, "x")?,
            y: num(o, "y")?,
            w: num(o, "w")?,
            h: num(o, "h")?,
        }),
        _ => None,
    };
    Some(Probe {
        target: v.get("target")?.as_str()?.to_string(),
        ids: strings("ids").into_iter().collect(),
        rect,
        viewport: Viewport {
            w: num(v, "vw")?,
            h: num(v, "vh")?,
        },
        gate: tour_gate(flag("reason")?, flag("overlay")?, flag("modal")?),
        clicks: strings("clicks"),
        esc: flag("esc")?,
    })
}

// ── The handle ───────────────────────────────────────────────────────────────────────────

/// The walkthrough's state, shared by context. `Copy`: it is signals.
#[derive(Clone, Copy)]
pub(crate) struct TourHandle {
    tour: Signal<Tour>,
    prefs: Signal<TourPrefs>,
    /// The remembered settings have been read.
    loaded: Signal<bool>,
    /// The popup should take the keyboard when it next shows (set by a start).
    focus_pending: Signal<bool>,
    toasts: Signal<Vec<crate::toast::Toast>>,
}

/// Create the walkthrough's state and provide it to everything below. Call once, from the shell.
pub(crate) fn provide_tour() -> TourHandle {
    let tour = use_signal(Tour::new);
    let prefs = use_signal(TourPrefs::default);
    let loaded = use_signal(|| false);
    let focus_pending = use_signal(|| false);
    let own_toasts = use_signal(Vec::<crate::toast::Toast>::new);
    let toasts = try_use_context::<Signal<Vec<crate::toast::Toast>>>().unwrap_or(own_toasts);
    use_context_provider(|| TourHandle {
        tour,
        prefs,
        loaded,
        focus_pending,
        toasts,
    })
}

impl TourHandle {
    /// Whether the walkthrough is running.
    pub(crate) fn is_active(&self) -> bool {
        self.tour.read().is_active()
    }

    /// Whether the "Guided inspection" switch is on.
    pub(crate) fn guided(&self) -> bool {
        self.prefs.read().guided
    }

    /// THE ONE START. The Help menu, the command palette, the switch and the offer all call this.
    /// It asks the page what owns the screen, and a reason prompt, an overlay or a dialog refuses
    /// it, with the reason (like the palette and the stacked view are refused). Otherwise the
    /// walkthrough starts at step 1, the switch turns on, and the popup takes the keyboard.
    pub(crate) fn start(self) {
        spawn(async move {
            let gate = read_gate().await;
            self.start_with_gate(gate);
        });
    }

    /// Start, given what owns the screen. Answers whether it started.
    pub(crate) fn start_with_gate(mut self, gate: TourGate) -> bool {
        if let Some(why) = gate.refusal() {
            crate::toast::push_toast(self.toasts, crate::toast::ToastKind::Info, why);
            return false;
        }
        self.tour.write().start();
        let mut prefs = *self.prefs.peek();
        prefs.guided = true;
        // Starting is also an answer to the first-launch offer.
        if prefs.offer == Offer::Pending {
            prefs.offer = Offer::Accepted;
            persist(KEY_OFFER, prefs.offer.as_word());
        }
        self.prefs.set(prefs);
        persist(KEY_GUIDED, guided_word(true));
        self.focus_pending.set(true);
        true
    }

    /// Stop. The switch turns off, so it stays off next launch. `restore_focus` puts the keyboard
    /// back on the control the popup was pointing at (Exit, Done and Esc); turning the switch off
    /// from the switch itself leaves it where it is.
    pub(crate) fn exit(mut self, restore_focus: bool) {
        let target = self.tour.peek().step().map(|s| s.target);
        self.tour.write().exit();
        self.set_guided_off();
        if restore_focus {
            if let Some(target) = target {
                let _ = dioxus::document::eval(&focus_control_js(target));
            }
        }
    }

    /// The Next button. On the last step it ends the walkthrough like Exit.
    fn next(mut self) {
        let target = self.tour.peek().step().map(|s| s.target);
        let progress = self.tour.write().go_next();
        if progress == Progress::Finished {
            self.set_guided_off();
            if let Some(target) = target {
                let _ = dioxus::document::eval(&focus_control_js(target));
            }
        }
    }

    /// The Back button.
    fn back(mut self) {
        self.tour.write().back();
    }

    fn set_guided_off(mut self) {
        let mut prefs = *self.prefs.peek();
        prefs.guided = false;
        self.prefs.set(prefs);
        persist(KEY_GUIDED, guided_word(false));
    }

    /// "No thanks" on the first-launch offer. Remembered, so it is not offered again.
    pub(crate) fn decline_offer(mut self) {
        let mut prefs = *self.prefs.peek();
        prefs.offer = Offer::Declined;
        self.prefs.set(prefs);
        persist(KEY_OFFER, prefs.offer.as_word());
    }

    /// The remembered settings were read.
    fn apply_loaded(mut self, prefs: TourPrefs) {
        self.prefs.set(prefs);
        self.loaded.set(true);
        // The switch was left on: the walkthrough runs again from the first step.
        if prefs.guided && !self.tour.peek().is_active() {
            self.tour.write().start();
        }
    }
}

/// Remember one word.
fn persist(key: &str, value: &str) {
    let _ = dioxus::document::eval(&write_pref_js(key, value));
}

/// What the page says owns the screen. A page that does not answer is taken to have nothing up.
async fn read_gate() -> TourGate {
    let mut ev = dioxus::document::eval(&gate_js());
    let Ok(v) = ev.recv::<serde_json::Value>().await else {
        return TourGate::Open;
    };
    let flag = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    tour_gate(flag("reason"), flag("overlay"), flag("modal"))
}

// ── The Help menu ────────────────────────────────────────────────────────────────────────

/// Hears the Help menu's "Start walkthrough" and calls [`TourHandle::start`], the function the
/// palette's line calls. Renders nothing.
#[component]
pub(crate) fn HelpMenuListener() -> Element {
    let handle = try_use_context::<TourHandle>();
    dioxus::desktop::use_muda_event_handler(move |event| {
        if event.id().0 == camerata_ui::desktop_chrome::HELP_START_WALKTHROUGH_ID {
            if let Some(handle) = handle {
                handle.start();
            }
        }
    });
    rsx! {}
}

// ── The switch and the offer ─────────────────────────────────────────────────────────────

/// The "Guided inspection" switch, where an inspection starts. On: the walkthrough runs alongside
/// the real flow. Off: it is hidden. Remembered per user. Renders nothing where no walkthrough
/// state has been provided.
#[component]
pub(crate) fn GuidedSwitch() -> Element {
    let Some(handle) = try_use_context::<TourHandle>() else {
        return rsx! {};
    };
    let on = handle.guided() && handle.is_active();
    rsx! {
        div { class: "tour-switch-row",
            button {
                class: if on { "tour-switch tour-switch-on" } else { "tour-switch" },
                r#type: "button",
                role: "switch",
                "aria-checked": if on { "true" } else { "false" },
                title: "Show a short guided walkthrough alongside the real inspection flow.",
                onclick: move |_| {
                    if on {
                        handle.exit(false);
                    } else {
                        handle.start();
                    }
                },
                span { class: "tour-switch-knob" }
            }
            span { class: "tour-switch-label", "Guided inspection" }
            span { class: "tour-switch-hint",
                if on { "On. A popup points at each control." } else { "Off." }
            }
        }
    }
}

/// The one-time offer on a first launch with no projects. `projects` is the number of projects,
/// or `None` while the list loads. Renders nothing after either answer, or where no walkthrough
/// state has been provided.
#[component]
pub(crate) fn WalkthroughOffer(projects: Option<usize>) -> Element {
    let Some(handle) = try_use_context::<TourHandle>() else {
        return rsx! {};
    };
    if !should_offer(*handle.loaded.read(), *handle.prefs.read(), projects) {
        return rsx! {};
    }
    rsx! {
        div {
            class: "tour-offer",
            role: "region",
            "aria-label": "Guided inspection offer",
            p { class: "tour-offer-q", "Want a guided first inspection?" }
            p { class: "tour-offer-sub",
                "A small popup points at each control, from creating a project to exporting the package. You can turn it off at any time."
            }
            div { class: "tour-offer-actions",
                button {
                    class: "btn-edit-sm",
                    r#type: "button",
                    onclick: move |_| handle.decline_offer(),
                    "No thanks"
                }
                button {
                    class: "btn-run",
                    r#type: "button",
                    onclick: move |_| handle.start(),
                    "Guide me"
                }
            }
        }
    }
}

// ── The popup ────────────────────────────────────────────────────────────────────────────

/// What the popup looks like for the current look at the page.
fn mode_of(tour: &Tour, probe: Option<&Probe>) -> PopupMode {
    let Some(step) = tour.step() else {
        return PopupMode::Hidden;
    };
    // A look for another step is stale: wait for the next.
    let Some(probe) = probe.filter(|p| p.target == step.target) else {
        return PopupMode::Hidden;
    };
    popup_mode(true, probe.gate, probe.rect.is_some())
}

/// Look at the page while the walkthrough runs, hand what it sees to the tour, and draw the popup
/// and the highlight. Mount once, beside the shell. Renders nothing while the walkthrough is off.
#[component]
pub(crate) fn WalkthroughHost() -> Element {
    let Some(handle) = try_use_context::<TourHandle>() else {
        return rsx! {};
    };
    let tour = handle.tour;
    let mut probe = use_signal(|| Option::<Probe>::None);

    // Read what was remembered.
    use_future(move || async move {
        let mut ev = dioxus::document::eval(&read_prefs_js());
        let prefs = match ev.recv::<serde_json::Value>().await {
            Ok(v) => parse_prefs(
                v.get("guided").and_then(serde_json::Value::as_str),
                v.get("offer").and_then(serde_json::Value::as_str),
            ),
            // No storage to read: the defaults, so the offer can still be made.
            Err(_) => TourPrefs::default(),
        };
        handle.apply_loaded(prefs);
    });

    // Look at the page while it runs.
    use_future(move || async move {
        let mut last_step: Option<&'static str> = None;
        let mut again_now = false;
        loop {
            if !again_now {
                tokio::time::sleep(Duration::from_millis(PROBE_MS)).await;
            }
            again_now = false;
            let Some(step) = tour.peek().step() else {
                last_step = None;
                if probe.peek().is_some() {
                    probe.set(None);
                }
                continue;
            };
            let scroll = last_step != Some(step.id);
            last_step = Some(step.id);
            let mut ev = dioxus::document::eval(&probe_js(step.target, scroll));
            let Ok(raw) = ev.recv::<serde_json::Value>().await else {
                continue;
            };
            let Some(seen) = parse_probe(&raw) else {
                continue;
            };
            let before = tour.peek().step().map(|s| s.id);
            let mut writable = tour;
            let moved = apply_probe(&mut writable.write(), &seen);
            if seen.esc && seen.gate.is_open() && tour.peek().is_active() {
                handle.exit(true);
                continue;
            }
            if moved || tour.peek().step().map(|s| s.id) != before {
                // The step changed: look again at once, for the new control.
                again_now = true;
            }
            probe.set(Some(seen));
        }
    });

    let mode = use_memo(move || mode_of(&tour.read(), probe.read().as_ref()));

    // A start moves the keyboard into the popup, once it shows.
    use_effect(move || {
        if mode() != PopupMode::Hidden && *handle.focus_pending.peek() {
            let mut pending = handle.focus_pending;
            pending.set(false);
            let _ = dioxus::document::eval(&focus_popup_js());
        }
    });

    let snapshot = tour.read().clone();
    let (Some(step), Some((n, m))) = (snapshot.step(), snapshot.position()) else {
        return rsx! {};
    };
    let current = probe.read().clone().filter(|p| p.target == step.target);
    let mode = mode();
    // Read out when the step changes or the popup comes back; empty while hidden so it is never
    // read over a dialog.
    let announcement = match mode {
        PopupMode::Hidden => String::new(),
        PopupMode::Pointing => format!("Step {n} of {m}. {}. {}", step.title, step.body),
        PopupMode::Directions => format!(
            "Step {n} of {m}. {}. {} {}",
            step.title, step.body, step.how_to_get_there
        ),
    };

    let next_label = snapshot.next_label();
    let can_back = snapshot.can_back();

    let (ring_style, pop_style, pop_class, arrow_style) = match (mode, &current) {
        (PopupMode::Pointing, Some(p)) => {
            let rect = p.rect.unwrap_or_default();
            let placement = place(rect, p.viewport);
            let ring = format!(
                "left:{}px;top:{}px;width:{}px;height:{}px;",
                rect.x - 4.0,
                rect.y - 4.0,
                rect.w + 8.0,
                rect.h + 8.0
            );
            let (pop, class) = match placement.side {
                Side::Below => (
                    format!("left:{}px;top:{}px;", placement.left, placement.edge),
                    "tour-pop tour-below",
                ),
                Side::Above => (
                    format!("left:{}px;bottom:{}px;", placement.left, placement.edge),
                    "tour-pop tour-above",
                ),
            };
            (ring, pop, class, format!("left:{}px;", placement.arrow))
        }
        _ => (
            String::new(),
            String::new(),
            "tour-pop tour-corner",
            String::new(),
        ),
    };

    rsx! {
        div { class: "tour-sr", role: "status", "aria-live": "polite", "{announcement}" }
        if mode != PopupMode::Hidden {
            if mode == PopupMode::Pointing {
                div { class: "tour-ring", style: "{ring_style}" }
            }
            div {
                id: POPUP_ID,
                class: "{pop_class}",
                style: "{pop_style}",
                role: "dialog",
                tabindex: "-1",
                "aria-labelledby": "camerata-tour-title",
                "aria-describedby": "camerata-tour-body",
                if mode == PopupMode::Pointing {
                    span { class: "tour-arrow", style: "{arrow_style}" }
                }
                div { class: "tour-head",
                    span { class: "tour-count", "Step {n} of {m}" }
                }
                p { id: "camerata-tour-title", class: "tour-title", "{step.title}" }
                p { id: "camerata-tour-body", class: "tour-body", "{step.body}" }
                if mode == PopupMode::Directions {
                    p { class: "tour-how", "{step.how_to_get_there}" }
                }
                div { class: "tour-actions",
                    button {
                        class: "btn-edit-sm tour-exit",
                        r#type: "button",
                        title: "Close the walkthrough (Esc)",
                        onclick: move |_| handle.exit(true),
                        "Exit"
                    }
                    span { class: "tour-spacer" }
                    button {
                        class: "btn-edit-sm tour-back",
                        r#type: "button",
                        disabled: !can_back,
                        onclick: move |_| handle.back(),
                        "Back"
                    }
                    button {
                        class: "btn-run tour-next",
                        r#type: "button",
                        onclick: move |_| handle.next(),
                        "{next_label}"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_ui_core::walkthrough::target;
    use serde_json::json;

    fn sent(overrides: serde_json::Value) -> serde_json::Value {
        let mut base = json!({
            "target": target::SETTINGS,
            "ids": [target::SETTINGS, target::NAV_ONBOARD],
            "rect": { "x": 10.0, "y": 20.0, "w": 100.0, "h": 30.0 },
            "vw": 1200.0,
            "vh": 800.0,
            "reason": false,
            "overlay": false,
            "modal": false,
            "clicks": [],
            "esc": false,
        });
        if let (Some(b), Some(o)) = (base.as_object_mut(), overrides.as_object()) {
            for (k, v) in o {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    }

    // ── the page scripts ─────────────────────────────────────────────────────────────────

    #[test]
    fn the_probe_names_its_target_and_the_attribute_and_reports_everything_the_tour_needs() {
        let js = probe_js(target::RUN_INSPECTION, true);
        assert!(js.contains("\"run-inspection\""));
        assert!(js.contains("[data-tour]"));
        for field in [
            "ids:", "rect:", "vw:", "vh:", "reason:", "overlay:", "modal:", "clicks:", "esc:",
        ] {
            assert!(js.contains(field), "{field}");
        }
        assert!(js.contains("if (true && outside)"), "{js}");
        assert!(probe_js("x", false).contains("if (false && outside)"));
    }

    #[test]
    fn the_listeners_never_stop_prevent_or_consume_an_event() {
        let js = install_script();
        assert!(!js.contains("preventDefault"));
        assert!(!js.contains("stopPropagation"));
        assert!(!js.contains("stopImmediatePropagation"));
        // Installed once, in the capture phase.
        assert!(js.contains("__camerataTourInstalled"));
        assert_eq!(js.matches("}, true);").count(), 2);
    }

    #[test]
    fn esc_is_not_taken_from_a_reason_prompt_an_overlay_a_dialog_or_a_field() {
        let js = install_script();
        assert!(js.contains(REASON_SELECTOR));
        assert!(js.contains(OVERLAY_SELECTOR));
        assert!(js.contains(MODAL_SELECTOR));
        for tag in ["input", "textarea", "select"] {
            assert!(js.contains(&format!("\"{tag}\"")), "{tag}");
        }
    }

    #[test]
    fn the_gate_selectors_name_the_surfaces_the_cockpit_draws() {
        // The viewer and the stacked view are `cv-overlay`, the palette is `pal-overlay`, the
        // reason prompts have their own classes, and every other dialog is a rule-modal-overlay.
        assert!(OVERLAY_SELECTOR.contains(".cv-overlay"));
        assert!(OVERLAY_SELECTOR.contains(".pal-overlay"));
        assert!(REASON_SELECTOR.contains(".queue-reason-prompt"));
        assert!(REASON_SELECTOR.contains(".pane-reason-prompt"));
        assert!(MODAL_SELECTOR.starts_with(".rule-modal-overlay"));
    }

    #[test]
    fn the_start_gate_does_not_count_the_palette_that_is_running_the_line() {
        let js = gate_js();
        // The overlay flag is the viewer and the stack only; the palette appears in the dialog
        // flag solely as an exclusion.
        assert!(
            js.contains("overlay: !!document.querySelector(\".cv-overlay\")"),
            "{js}"
        );
        assert!(js.contains(":not(.pal-overlay)"), "{js}");
        assert!(js.contains(".cv-overlay"));
        assert!(js.contains(REASON_SELECTOR));
    }

    #[test]
    fn the_stored_words_are_written_and_read_under_the_core_keys() {
        assert!(read_prefs_js().contains(KEY_GUIDED));
        assert!(read_prefs_js().contains(KEY_OFFER));
        let w = write_pref_js(KEY_GUIDED, "true");
        assert!(w.contains("\"camerata.tour.guided\""));
        assert!(w.contains("\"true\""));
        assert!(w.contains("try {"), "storage may be unavailable");
    }

    #[test]
    fn a_quote_in_a_stored_word_cannot_break_out_of_its_string() {
        let w = write_pref_js("k", "a\"b");
        assert!(w.contains("\"a\\\"b\""), "{w}");
    }

    #[test]
    fn the_keyboard_goes_back_to_the_control_or_the_first_thing_inside_it() {
        let js = focus_control_js(target::REPO_ADD);
        assert!(js.contains("\"repo-add\""));
        assert!(js.contains("el.matches(focusable) ? el : el.querySelector(focusable)"));
        assert!(js.contains("to.focus()"));
        assert!(focus_popup_js().contains(POPUP_ID));
    }

    // ── what the page said ───────────────────────────────────────────────────────────────

    #[test]
    fn a_probe_parses_into_what_the_tour_needs() {
        let p = parse_probe(&sent(json!({ "clicks": ["settings"], "esc": true }))).expect("parses");
        assert_eq!(p.target, "settings");
        assert!(p.ids.contains("nav-onboard"));
        assert_eq!(
            p.rect,
            Some(Rect {
                x: 10.0,
                y: 20.0,
                w: 100.0,
                h: 30.0
            })
        );
        assert_eq!(
            p.viewport,
            Viewport {
                w: 1200.0,
                h: 800.0
            }
        );
        assert_eq!(p.gate, TourGate::Open);
        assert_eq!(p.clicks, vec!["settings".to_string()]);
        assert!(p.esc);
    }

    #[test]
    fn a_control_that_is_not_on_screen_has_no_rect() {
        let p = parse_probe(&sent(json!({ "rect": null }))).expect("parses");
        assert_eq!(p.rect, None);
    }

    #[test]
    fn what_owns_the_screen_becomes_the_gate_in_the_documented_order() {
        let gate = |r, o, m| {
            parse_probe(&sent(json!({ "reason": r, "overlay": o, "modal": m })))
                .expect("parses")
                .gate
        };
        assert_eq!(gate(false, false, true), TourGate::Modal);
        assert_eq!(gate(false, true, true), TourGate::Overlay);
        assert_eq!(gate(true, true, true), TourGate::ReasonPrompt);
    }

    #[test]
    fn a_malformed_probe_is_dropped_not_guessed_at() {
        assert!(parse_probe(&json!({})).is_none());
        assert!(parse_probe(&json!({ "target": "x" })).is_none());
        assert!(parse_probe(&sent(json!({ "vw": "wide" }))).is_none());
        assert!(parse_probe(&sent(json!({ "rect": { "x": 1 } }))).is_none());
    }

    // ── the mode ─────────────────────────────────────────────────────────────────────────

    fn tour_on_step(id: &str) -> Tour {
        let mut tour = Tour::new();
        tour.start();
        while tour.step().is_some_and(|s| s.id != id) {
            tour.go_next();
        }
        tour
    }

    fn look(for_target: &str, rect: bool, gate: serde_json::Value) -> Probe {
        let mut v = sent(gate);
        v["target"] = json!(for_target);
        if !rect {
            v["rect"] = serde_json::Value::Null;
        }
        parse_probe(&v).expect("parses")
    }

    #[test]
    fn nothing_is_drawn_until_the_page_has_been_looked_at_for_this_step() {
        let tour = tour_on_step("backend");
        assert_eq!(mode_of(&tour, None), PopupMode::Hidden);
        let stale = look(target::REPO_ADD, true, json!({}));
        assert_eq!(mode_of(&tour, Some(&stale)), PopupMode::Hidden);
        let fresh = look(target::SETTINGS, true, json!({}));
        assert_eq!(mode_of(&tour, Some(&fresh)), PopupMode::Pointing);
    }

    #[test]
    fn a_missing_control_gets_directions_not_a_pointer() {
        let tour = tour_on_step("backend");
        let gone = look(target::SETTINGS, false, json!({}));
        assert_eq!(mode_of(&tour, Some(&gone)), PopupMode::Directions);
    }

    #[test]
    fn a_reason_prompt_an_overlay_or_a_dialog_hides_it() {
        let tour = tour_on_step("queue");
        for gate in [
            json!({ "reason": true }),
            json!({ "overlay": true }),
            json!({ "modal": true }),
        ] {
            let p = look(target::REVIEW_QUEUE, true, gate);
            assert_eq!(mode_of(&tour, Some(&p)), PopupMode::Hidden);
            let p = look(target::REVIEW_QUEUE, false, json!({ "modal": true }));
            assert_eq!(mode_of(&tour, Some(&p)), PopupMode::Hidden);
        }
    }

    #[test]
    fn an_off_tour_draws_nothing() {
        let p = look(target::SETTINGS, true, json!({}));
        assert_eq!(mode_of(&Tour::new(), Some(&p)), PopupMode::Hidden);
    }

    #[test]
    fn the_menu_id_is_namespaced_and_the_label_is_the_palettes() {
        assert!(camerata_ui::desktop_chrome::HELP_START_WALKTHROUGH_ID.starts_with("camerata."));
        assert_eq!(
            camerata_ui_core::palette::WALKTHROUGH_LABEL,
            "Start walkthrough"
        );
    }

    // ── rendering ────────────────────────────────────────────────────────────────────────

    fn render(harness: fn() -> Element) -> String {
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    #[test]
    fn the_switch_and_the_offer_draw_nothing_without_a_walkthrough() {
        fn harness() -> Element {
            rsx! {
                GuidedSwitch {}
                WalkthroughOffer { projects: Some(0usize) }
                WalkthroughHost {}
            }
        }
        let html = render(harness);
        assert!(!html.contains("Guided inspection"), "{html}");
        assert!(!html.contains("tour-"), "{html}");
    }

    #[test]
    fn the_switch_starts_off_and_says_so() {
        fn harness() -> Element {
            provide_tour();
            rsx! { GuidedSwitch {} }
        }
        let html = render(harness);
        assert!(html.contains("Guided inspection"), "{html}");
        assert!(html.contains("role=\"switch\""), "{html}");
        assert!(html.contains("aria-checked=\"false\""), "{html}");
        assert!(!html.contains("tour-switch-on"));
    }

    #[test]
    fn the_offer_waits_for_the_settings_and_the_project_list() {
        fn loading() -> Element {
            provide_tour();
            rsx! { WalkthroughOffer { projects: None } }
        }
        // Settings not read yet: no offer.
        assert!(!render(loading).contains("Want a guided first inspection?"));
    }

    #[test]
    fn the_offer_shows_once_loaded_with_no_projects_and_not_with_one() {
        fn empty() -> Element {
            let handle = provide_tour();
            use_hook(|| handle.apply_loaded(TourPrefs::default()));
            rsx! { WalkthroughOffer { projects: Some(0usize) } }
        }
        fn one() -> Element {
            let handle = provide_tour();
            use_hook(|| handle.apply_loaded(TourPrefs::default()));
            rsx! { WalkthroughOffer { projects: Some(1usize) } }
        }
        fn declined() -> Element {
            let handle = provide_tour();
            use_hook(|| {
                handle.apply_loaded(TourPrefs {
                    offer: Offer::Declined,
                    ..TourPrefs::default()
                })
            });
            rsx! { WalkthroughOffer { projects: Some(0usize) } }
        }
        let shown = render(empty);
        assert!(shown.contains("Want a guided first inspection?"), "{shown}");
        assert!(shown.contains("No thanks"));
        assert!(shown.contains("Guide me"));
        assert!(!render(one).contains("Want a guided first inspection?"));
        assert!(!render(declined).contains("Want a guided first inspection?"));
    }

    #[test]
    fn a_walkthrough_left_on_starts_when_the_settings_load() {
        fn harness() -> Element {
            let handle = provide_tour();
            use_hook(|| {
                handle.apply_loaded(TourPrefs {
                    guided: true,
                    ..TourPrefs::default()
                })
            });
            rsx! { GuidedSwitch {} }
        }
        let html = render(harness);
        assert!(html.contains("aria-checked=\"true\""), "{html}");
        assert!(html.contains("tour-switch-on"), "{html}");
    }

    #[test]
    fn a_start_under_a_reason_prompt_is_refused_and_says_why() {
        fn harness() -> Element {
            let handle = provide_tour();
            let started = use_hook(|| handle.start_with_gate(TourGate::ReasonPrompt));
            let toasts = handle.toasts.read();
            let said = toasts
                .iter()
                .map(|t| t.message.clone())
                .collect::<Vec<_>>()
                .join("|");
            rsx! { p { "started={started} active={handle.is_active()} guided={handle.guided()} said={said}" } }
        }
        let html = render(harness);
        assert!(html.contains("started=false"), "{html}");
        assert!(html.contains("active=false"), "{html}");
        assert!(html.contains("guided=false"), "{html}");
        assert!(html.contains("Finish or cancel the reason first"), "{html}");
    }

    #[test]
    fn a_start_under_an_overlay_or_a_dialog_is_refused_too() {
        fn overlay() -> Element {
            let handle = provide_tour();
            let started = use_hook(|| handle.start_with_gate(TourGate::Overlay));
            rsx! { p { "started={started} active={handle.is_active()}" } }
        }
        fn modal() -> Element {
            let handle = provide_tour();
            let started = use_hook(|| handle.start_with_gate(TourGate::Modal));
            rsx! { p { "started={started} active={handle.is_active()}" } }
        }
        for html in [render(overlay), render(modal)] {
            assert!(html.contains("started=false active=false"), "{html}");
        }
    }

    #[test]
    fn an_open_gate_starts_it_turns_the_switch_on_and_answers_the_offer() {
        fn harness() -> Element {
            let handle = provide_tour();
            let started = use_hook(|| handle.start_with_gate(TourGate::Open));
            let offer = handle.prefs.read().offer;
            rsx! { p { "started={started} active={handle.is_active()} guided={handle.guided()} offer={offer:?}" } }
        }
        let html = render(harness);
        assert!(
            html.contains("started=true active=true guided=true offer=Accepted"),
            "{html}"
        );
    }

    #[test]
    fn exit_turns_the_switch_off_and_stops_the_tour() {
        fn harness() -> Element {
            let handle = provide_tour();
            use_hook(|| {
                handle.start_with_gate(TourGate::Open);
                handle.exit(true);
            });
            rsx! { p { "active={handle.is_active()} guided={handle.guided()}" } }
        }
        let html = render(harness);
        assert!(html.contains("active=false guided=false"), "{html}");
    }

    #[test]
    fn declining_the_offer_is_remembered_and_leaves_the_switch_alone() {
        fn harness() -> Element {
            let handle = provide_tour();
            use_hook(|| {
                handle.apply_loaded(TourPrefs::default());
                handle.decline_offer();
            });
            let offer = handle.prefs.read().offer;
            rsx! { p { "offer={offer:?} guided={handle.guided()} active={handle.is_active()}" } }
        }
        let html = render(harness);
        assert!(
            html.contains("offer=Declined guided=false active=false"),
            "{html}"
        );
    }

    #[test]
    fn the_host_draws_only_a_quiet_live_region_before_the_page_has_been_looked_at() {
        fn harness() -> Element {
            let handle = provide_tour();
            use_hook(|| handle.start_with_gate(TourGate::Open));
            rsx! { WalkthroughHost {} }
        }
        let html = render(harness);
        // The status region is there from the start so a screen reader hears the first step;
        // no popup is drawn until the page has been looked at (nothing to point at yet).
        assert!(html.contains("role=\"status\""), "{html}");
        assert!(html.contains("aria-live=\"polite\""), "{html}");
        assert!(!html.contains("camerata-tour-popup"), "{html}");
    }
}
