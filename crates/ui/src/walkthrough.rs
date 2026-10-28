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
//! besides drawing is scroll the current control into view once per step (never behind a dialog
//! or the palette), and move the keyboard (into the popup when the walkthrough is started; back to
//! the popup if a step change dropped it; back to the highlighted control when it is closed with
//! Exit, Done or Esc, and then onto the control itself, never onto something inside it).
//!
//! The page-side listeners (clicks, Esc) record only while a walkthrough is running: they are
//! switched on at the start and off at the exit, and their buffers are emptied at each start, so
//! a stray Esc or click between walkthroughs can never act on the next one.
//!
//! Remembered per user in the webview's own storage, the same store as the background animation
//! switch, and never in a project (so no project export carries it).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use camerata_ui_core::walkthrough::{
    guided_word, parse_prefs, place, popup_mode, should_offer, start_gate, tour_gate, Offer,
    PopupMode, Progress, Rect, Side, StartSource, Tour, TourGate, TourPrefs, Viewport, KEY_GUIDED,
    KEY_OFFER, TOUR_ATTRIBUTE,
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
/// consumes an event, and both record ONLY while `window.__camerataTourActive` is set (a
/// walkthrough is running); the click buffer is capped.
pub(crate) fn install_script() -> String {
    format!(
        r#"if (!window.__camerataTourInstalled) {{
  window.__camerataTourInstalled = true;
  if (typeof window.__camerataTourActive === "undefined") window.__camerataTourActive = false;
  window.__camerataTourClicks = window.__camerataTourClicks || [];
  window.__camerataTourEsc = !!window.__camerataTourEsc;
  document.addEventListener("click", function (e) {{
    if (!window.__camerataTourActive) return;
    var t = e.target && e.target.closest ? e.target.closest("[{TOUR_ATTRIBUTE}]") : null;
    if (t && window.__camerataTourClicks.length < {MAX_RECORDED_CLICKS}) {{
      window.__camerataTourClicks.push(t.getAttribute("{TOUR_ATTRIBUTE}"));
    }}
  }}, true);
  document.addEventListener("keydown", function (e) {{
    if (!window.__camerataTourActive) return;
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

/// The most clicks held between two looks.
const MAX_RECORDED_CLICKS: usize = 50;

/// A walkthrough is starting: install the listeners, empty what they hold (nothing recorded
/// before this moment counts), and switch them on.
pub(crate) fn begin_js() -> String {
    let install = install_script();
    format!(
        r#"{install}
window.__camerataTourClicks = [];
window.__camerataTourEsc = false;
window.__camerataTourActive = true;"#
    )
}

/// A walkthrough ended: switch the listeners off and empty them. Safe to run when nothing was
/// ever installed.
pub(crate) fn end_js() -> &'static str {
    r#"window.__camerataTourActive = false;
window.__camerataTourClicks = [];
window.__camerataTourEsc = false;"#
}

/// One look at the page for the step whose control is `target`. Answers (to `dioxus.send`) the
/// `data-tour` ids on screen, the control's rectangle, the window size, what owns the screen, the
/// ids clicked since the last look, whether Esc was pressed, and whether the keyboard is inside
/// the popup. When `scroll` is set it first scrolls the control into view if it is outside the
/// window, but only when nothing owns the screen (never behind a dialog or the palette).
pub(crate) fn probe_js(target: &str, scroll: bool) -> String {
    let target_json = serde_json::to_string(target).unwrap_or_else(|_| "\"\"".to_string());
    let install = install_script();
    format!(
        r#"{install}
window.__camerataTourActive = true;
var target = {target_json};
var visible = function (el) {{
  var r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return false;
  var s = window.getComputedStyle(el);
  return s.visibility !== "hidden" && s.display !== "none";
}};
var reason = !!document.querySelector("{REASON_SELECTOR}");
var overlay = !!document.querySelector("{OVERLAY_SELECTOR}");
var modal = !!document.querySelector("{MODAL_SELECTOR}");
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
  if ({scroll} && outside && !(reason || overlay || modal)) {{
    found.scrollIntoView({{ block: "center", inline: "nearest" }});
    r = found.getBoundingClientRect();
  }}
  rect = {{ x: r.left, y: r.top, w: r.width, h: r.height }};
}}
var clicks = window.__camerataTourClicks; window.__camerataTourClicks = [];
var esc = window.__camerataTourEsc; window.__camerataTourEsc = false;
var pop = document.getElementById("{POPUP_ID}");
dioxus.send({{
  target: target,
  ids: ids,
  rect: rect,
  vw: window.innerWidth,
  vh: window.innerHeight,
  reason: reason,
  overlay: overlay,
  modal: modal,
  clicks: clicks,
  esc: esc,
  focus_in_popup: !!(pop && pop.contains(document.activeElement))
}});"#
    )
}

/// What owns the screen right now, asked of the page: used to refuse starting the walkthrough
/// under a reason prompt, the code viewer or the stacked view, the palette, or a dialog. The
/// palette is reported on its own, so that the palette's own line (which runs while the palette
/// is open and closes it) can be told apart from the Help menu (which must not start under it).
fn gate_js() -> String {
    format!(
        r#"dioxus.send({{
  reason: !!document.querySelector("{REASON_SELECTOR}"),
  overlay: !!document.querySelector(".cv-overlay"),
  palette: !!document.querySelector(".pal-overlay"),
  modal: !!document.querySelector("{MODAL_SELECTOR}")
}});"#
    )
}

/// The start gate from what the page said, for the source of the request.
fn parse_gate(v: &serde_json::Value, source: StartSource) -> TourGate {
    let flag = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    start_gate(
        flag("reason"),
        flag("overlay"),
        flag("palette"),
        flag("modal"),
        source,
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

/// Write one remembered word, and say whether it was kept.
fn write_pref_js(key: &str, value: &str) -> String {
    let key = serde_json::to_string(key).unwrap_or_default();
    let value = serde_json::to_string(value).unwrap_or_default();
    format!(
        "try {{ localStorage.setItem({key}, {value}); dioxus.send(true); }} catch (e) {{ dioxus.send(false); }}"
    )
}

/// Put the keyboard on the popup.
fn focus_popup_js() -> String {
    format!("var p = document.getElementById(\"{POPUP_ID}\"); if (p) p.focus();")
}

/// A step change dropped the keyboard out of the popup: put it back on the popup's forward
/// button. Does nothing when the keyboard is still inside it (or the popup is not drawn).
fn refocus_popup_js() -> String {
    format!(
        r#"var p = document.getElementById("{POPUP_ID}");
if (p && !p.contains(document.activeElement)) {{
  var b = p.querySelector(".tour-next") || p;
  b.focus();
}}"#
    )
}

/// Whether to put the keyboard back in the popup after a look: the step changed, and the keyboard
/// was inside the popup when the look was taken.
pub(crate) fn refocus_after(step_changed: bool, focus_was_in_popup: bool) -> bool {
    step_changed && focus_was_in_popup
}

/// Put the keyboard back on the control the popup was pointing at, and ONLY that control. A
/// control that takes focus itself (a button, a field, a link) gets it. Anything else (a block, a
/// bar, a banner) gets focus on the block itself, made focusable for the purpose, and never on
/// something inside it: the first button inside the review queue's toolbar is Confirm, and a
/// reflex Enter must not decide a finding. Nothing when the control is gone.
pub(crate) fn focus_control_js(target: &str) -> String {
    let target = serde_json::to_string(target).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"var el = document.querySelector('[{TOUR_ATTRIBUTE}=' + JSON.stringify({target}) + ']');
if (el) {{
  if (!el.matches('button, [href], input, select, textarea') && !el.hasAttribute('tabindex')) {{
    el.setAttribute('tabindex', '-1');
  }}
  if (typeof el.focus === 'function') el.focus();
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
    /// The keyboard was inside the popup when the look was taken.
    pub focus_in_popup: bool,
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
        focus_in_popup: flag("focus_in_popup").unwrap_or(false),
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

    /// THE ONE START. The Help menu, the command palette, the switch and the offer all call this,
    /// saying where the request came from. It asks the page what owns the screen, and a reason
    /// prompt, the viewer or the stack, a dialog or the palette refuses it, with the reason (like
    /// the palette and the stacked view are refused); only the palette's own line may start it
    /// with the palette open. Otherwise the walkthrough starts at step 1, the switch turns on,
    /// and the popup takes the keyboard.
    pub(crate) fn start(self, source: StartSource) {
        spawn(async move {
            let gate = read_gate(source).await;
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

/// Set once the first storage write failure has been logged.
static WRITE_FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);

/// Whether to log a storage write failure now: the first one only.
fn first_write_failure(logged: &AtomicBool) -> bool {
    !logged.swap(true, Ordering::Relaxed)
}

/// Remember one word. A failed write (storage blocked or full) changes nothing the user sees; it
/// is logged once.
fn persist(key: &str, value: &str) {
    let js = write_pref_js(key, value);
    let key = key.to_string();
    spawn(async move {
        let mut ev = dioxus::document::eval(&js);
        if let Ok(false) = ev.recv::<bool>().await {
            if first_write_failure(&WRITE_FAILURE_LOGGED) {
                eprintln!(
                    "[camerata-ui] walkthrough: could not save `{key}` to the app's storage; the setting will not be remembered (this is logged once per process)"
                );
            }
        }
    });
}

/// What the page says owns the screen, for a start from `source`. A page that does not answer is
/// taken to have nothing up.
async fn read_gate(source: StartSource) -> TourGate {
    let mut ev = dioxus::document::eval(&gate_js());
    let Ok(v) = ev.recv::<serde_json::Value>().await else {
        return TourGate::Open;
    };
    parse_gate(&v, source)
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
                handle.start(StartSource::Elsewhere);
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
                        handle.start(StartSource::Elsewhere);
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
                    onclick: move |_| handle.start(StartSource::Elsewhere),
                    "Guide me"
                }
            }
        }
    }
}

// ── The popup ────────────────────────────────────────────────────────────────────────────

/// What the popup looks like for the latest look at the page.
///
/// After a step change the latest look was taken for the previous step's control until the next
/// look arrives (a fraction of a second). The popup keeps its mode and place through that gap, so
/// it does not blink out and back, and the new step's text is drawn in it. With no look at all
/// there is nothing to place it by, so nothing is drawn.
fn mode_of(tour: &Tour, probe: Option<&Probe>) -> PopupMode {
    if tour.step().is_none() {
        return PopupMode::Hidden;
    }
    let Some(probe) = probe else {
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
    let probe = use_signal(|| Option::<Probe>::None);

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

    // Look at the page while it runs. Restarts only when the walkthrough turns on or off (the
    // memo changes on nothing else), so while it is off nothing wakes at all.
    let running = use_memo(move || tour.read().is_active());
    let _looking = use_resource(move || {
        let on = running();
        async move {
            let mut probe = probe;
            if !on {
                // Off: switch the page's listeners off and empty them, and clear the last look.
                let _ = dioxus::document::eval(end_js());
                if probe.peek().is_some() {
                    probe.set(None);
                }
                return;
            }
            // On: nothing recorded before this moment counts.
            let _ = dioxus::document::eval(&begin_js());
            let mut scrolled_for: Option<&'static str> = None;
            let mut again_now = false;
            let mut refocus = false;
            loop {
                if !again_now {
                    tokio::time::sleep(Duration::from_millis(PROBE_MS)).await;
                }
                again_now = false;
                let Some(step) = tour.peek().step() else {
                    return;
                };
                let scroll = scrolled_for != Some(step.id);
                let mut ev = dioxus::document::eval(&probe_js(step.target, scroll));
                let Ok(raw) = ev.recv::<serde_json::Value>().await else {
                    continue;
                };
                let Some(seen) = parse_probe(&raw) else {
                    continue;
                };
                // The page scrolls only when nothing owns the screen; ask again until it could.
                if seen.gate.is_open() && seen.rect.is_some() {
                    scrolled_for = Some(step.id);
                }
                // The step change of the last look has been drawn by now: put the keyboard back
                // in the popup if that change dropped it.
                if refocus {
                    refocus = false;
                    let _ = dioxus::document::eval(&refocus_popup_js());
                }
                let before = step.id;
                let mut writable = tour;
                let moved = apply_probe(&mut writable.write(), &seen);
                if seen.esc && seen.gate.is_open() && tour.peek().is_active() {
                    handle.exit(true);
                    return;
                }
                let changed = moved || tour.peek().step().map(|s| s.id) != Some(before);
                if changed {
                    // The step changed: look again at once, for the new control.
                    again_now = true;
                    refocus = refocus_after(true, seen.focus_in_popup);
                }
                // Only a look that differs from the last one is news.
                if probe.peek().as_ref() != Some(&seen) {
                    probe.set(Some(seen));
                }
            }
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
    let current = probe.read().clone();
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
    use camerata_ui_core::walkthrough::{target, STEPS};
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
        // Scrolls only on request, and never behind a reason prompt, an overlay or a dialog.
        assert!(
            js.contains("if (true && outside && !(reason || overlay || modal))"),
            "{js}"
        );
        assert!(probe_js("x", false).contains("if (false && outside"));
        assert!(js.contains("focus_in_popup:"), "{js}");
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
    fn both_listeners_record_only_while_a_walkthrough_is_running_and_the_clicks_are_capped() {
        let js = install_script();
        // The first line of each handler is the switch.
        assert_eq!(
            js.matches("if (!window.__camerataTourActive) return;")
                .count(),
            2,
            "{js}"
        );
        assert!(js.contains(&format!(
            "window.__camerataTourClicks.length < {MAX_RECORDED_CLICKS}"
        )));
        // Starting empties what was held and switches on; ending empties and switches off.
        let begin = begin_js();
        assert!(begin.contains("__camerataTourInstalled"), "installs first");
        assert!(begin.contains("window.__camerataTourClicks = [];"));
        assert!(begin.contains("window.__camerataTourEsc = false;"));
        assert!(begin.ends_with("window.__camerataTourActive = true;"));
        assert!(end_js().contains("window.__camerataTourActive = false;"));
        assert!(end_js().contains("window.__camerataTourClicks = [];"));
        assert!(end_js().contains("window.__camerataTourEsc = false;"));
    }

    /// Run a script in node, when node is installed. `None` (the test then does not run its
    /// behavioural half) when it is not; the string checks beside each such test still run.
    fn node(script: &str) -> Option<serde_json::Value> {
        let probe = std::process::Command::new("node")
            .arg("--version")
            .output()
            .ok()?;
        if !probe.status.success() {
            return None;
        }
        let out = std::process::Command::new("node")
            .arg("-e")
            .arg(script)
            .output()
            .expect("node runs");
        assert!(
            out.status.success(),
            "node failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(serde_json::from_slice(&out.stdout).expect("the script printed json"))
    }

    #[test]
    fn an_esc_and_clicks_recorded_between_walkthroughs_have_no_effect_on_the_next_start() {
        let script = format!(
            r#"var handlers = {{}};
global.window = global;
global.document = {{
  addEventListener: function (t, f) {{ handlers[t] = f; }},
  querySelector: function () {{ return null; }},
  activeElement: null
}};
var click = function (id) {{
  handlers.click({{ target: {{ closest: function () {{ return {{ getAttribute: function () {{ return id; }} }}; }} }} }});
}};
var esc = function () {{ handlers.keydown({{ key: "Escape" }}); }};
var held = function () {{ return {{ clicks: window.__camerataTourClicks.length, esc: window.__camerataTourEsc }}; }};
var out = {{}};

// The page has just loaded: the listeners are installed (by a first start), then it ends.
{begin}
{end}
// A stray click and Esc between walkthroughs record nothing.
click("repo-add"); click("settings"); esc();
out.idle = held();
// And the next start begins empty, whatever happened before.
{begin}
out.after_start = held();
// While running they record, and the clicks stop at the cap.
click("settings"); esc();
out.running = held();
for (var i = 0; i < 200; i++) click("repo-add");
out.capped = window.__camerataTourClicks.length;
// Ending throws away what was held.
{end}
out.after_end = held();
console.log(JSON.stringify(out));"#,
            begin = begin_js(),
            end = end_js()
        );
        let Some(out) = node(&script) else {
            eprintln!("node is not installed: the behavioural half of this test was skipped");
            return;
        };
        assert_eq!(out["idle"], json!({ "clicks": 0, "esc": false }), "{out}");
        assert_eq!(
            out["after_start"],
            json!({ "clicks": 0, "esc": false }),
            "{out}"
        );
        assert_eq!(out["running"], json!({ "clicks": 1, "esc": true }), "{out}");
        assert_eq!(out["capped"], json!(MAX_RECORDED_CLICKS), "{out}");
        assert_eq!(
            out["after_end"],
            json!({ "clicks": 0, "esc": false }),
            "{out}"
        );
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
    fn the_start_gate_reports_the_palette_apart_from_the_viewer_and_the_stack() {
        let js = gate_js();
        assert!(
            js.contains("overlay: !!document.querySelector(\".cv-overlay\")"),
            "{js}"
        );
        assert!(
            js.contains("palette: !!document.querySelector(\".pal-overlay\")"),
            "{js}"
        );
        // A dialog is anything else: the palette is excluded from that flag.
        assert!(js.contains(":not(.pal-overlay)"), "{js}");
        assert!(js.contains(REASON_SELECTOR));
    }

    #[test]
    fn only_the_palettes_own_line_starts_the_walkthrough_with_the_palette_open() {
        let open = json!({ "reason": false, "overlay": false, "palette": true, "modal": false });
        assert_eq!(parse_gate(&open, StartSource::Elsewhere), TourGate::Overlay);
        assert_eq!(parse_gate(&open, StartSource::Palette), TourGate::Open);
        // The viewer and the stack refuse every source.
        let viewer = json!({ "overlay": true, "palette": true });
        assert_eq!(parse_gate(&viewer, StartSource::Palette), TourGate::Overlay);
        // A page that says nothing is taken to have nothing up.
        assert_eq!(
            parse_gate(&json!({}), StartSource::Elsewhere),
            TourGate::Open
        );
    }

    #[test]
    fn the_stored_words_are_written_and_read_under_the_core_keys() {
        assert!(read_prefs_js().contains(KEY_GUIDED));
        assert!(read_prefs_js().contains(KEY_OFFER));
        let w = write_pref_js(KEY_GUIDED, "true");
        assert!(w.contains("\"camerata.tour.guided\""));
        assert!(w.contains("\"true\""));
        assert!(w.contains("try {"), "storage may be unavailable");
        // It says whether the write was kept, so a failure can be logged.
        assert!(w.contains("dioxus.send(true)") && w.contains("dioxus.send(false)"));
    }

    #[test]
    fn a_storage_write_failure_is_logged_once() {
        let logged = AtomicBool::new(false);
        assert!(first_write_failure(&logged), "the first is logged");
        assert!(!first_write_failure(&logged), "the second is not");
        assert!(!first_write_failure(&logged));
    }

    #[test]
    fn a_quote_in_a_stored_word_cannot_break_out_of_its_string() {
        let w = write_pref_js("k", "a\"b");
        assert!(w.contains("\"a\\\"b\""), "{w}");
    }

    #[test]
    fn the_keyboard_goes_back_to_the_control_and_never_to_something_inside_it() {
        let js = focus_control_js(target::REPO_ADD);
        assert!(js.contains("\"repo-add\""));
        // Nothing is looked up inside the control, so nothing inside it can be focused.
        assert!(!js.contains("querySelector(focusable"), "{js}");
        assert_eq!(js.matches("querySelector").count(), 1, "{js}");
        assert!(js.contains("el.focus()"));
        assert!(focus_popup_js().contains(POPUP_ID));
    }

    #[test]
    fn a_step_change_that_dropped_the_keyboard_out_of_the_popup_puts_it_back() {
        assert!(refocus_after(true, true));
        assert!(!refocus_after(true, false), "the keyboard was elsewhere");
        assert!(!refocus_after(false, true), "the step did not change");
        let js = refocus_popup_js();
        assert!(js.contains(POPUP_ID));
        assert!(js.contains("!p.contains(document.activeElement)"));
        assert!(js.contains(".tour-next"));
    }

    #[test]
    fn closing_on_any_step_leaves_the_keyboard_on_the_control_never_on_a_button_inside_it() {
        // For every step's target: a block that holds a button (like the review queue's toolbar,
        // whose first button is Confirm). The script must focus the block, and looking inside it
        // throws. A control that takes focus itself gets it.
        let mut cases = String::new();
        for step in &STEPS {
            cases.push_str(&format!(
                r#"(function () {{
  var focused = null;
  var make = function (takesFocus) {{
    return {{
      matches: function () {{ return takesFocus; }},
      hasAttribute: function () {{ return false; }},
      setAttribute: function (k, v) {{ this.attr = k + "=" + v; }},
      focus: function () {{ focused = this; }},
      querySelector: function () {{ throw new Error("looked inside"); }}
    }};
  }};
  var block = make(false);
  var control = make(true);
  var current = block;
  global.document = {{ querySelector: function () {{ return current; }} }};
  {script}
  var onBlock = focused === block && block.attr === "tabindex=-1";
  focused = null; current = control;
  {script}
  results.push({{ target: {target:?}, onBlock: onBlock, onControl: focused === control && !control.attr }});
}})();
"#,
                script = focus_control_js(step.target),
                target = step.target
            ));
        }
        let script = format!(
            "var results = [];
{cases}console.log(JSON.stringify(results));"
        );
        let Some(out) = node(&script) else {
            eprintln!("node is not installed: the behavioural half of this test was skipped");
            return;
        };
        let rows = out.as_array().expect("one row per step");
        assert_eq!(rows.len(), STEPS.len());
        for row in rows {
            assert_eq!(row["onBlock"], json!(true), "{row}");
            assert_eq!(row["onControl"], json!(true), "{row}");
        }
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
    fn nothing_is_drawn_until_the_page_has_been_looked_at() {
        let tour = tour_on_step("backend");
        assert_eq!(mode_of(&tour, None), PopupMode::Hidden);
        let fresh = look(target::SETTINGS, true, json!({}));
        assert_eq!(mode_of(&tour, Some(&fresh)), PopupMode::Pointing);
    }

    #[test]
    fn a_step_change_keeps_the_popup_in_place_until_the_next_look_arrives() {
        // A harness probe sequence: look, click that advances the step, the (stale) look still
        // held, the fresh look for the new control.
        let mut tour = tour_on_step("backend");
        let first = look(target::SETTINGS, true, json!({}));
        assert_eq!(mode_of(&tour, Some(&first)), PopupMode::Pointing);

        let mut clicked = first.clone();
        clicked.clicks = vec![target::SETTINGS.to_string()];
        assert!(apply_probe(&mut tour, &clicked));
        assert_eq!(tour.step().map(|s| s.id), Some("rules"));
        // The look held is still the previous step's: the popup does not blink out.
        assert_eq!(mode_of(&tour, Some(&first)), PopupMode::Pointing);

        // The fresh look says the new control is not on screen: directions, in the same popup.
        let fresh = look(target::RULES, false, json!({}));
        assert_eq!(mode_of(&tour, Some(&fresh)), PopupMode::Directions);
        // And a dialog that opened in the gap hides it, as ever.
        let gated = look(target::RULES, true, json!({ "modal": true }));
        assert_eq!(mode_of(&tour, Some(&gated)), PopupMode::Hidden);
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
    fn the_help_menu_cannot_start_it_under_an_open_palette_but_the_palettes_own_line_can() {
        fn from_help() -> Element {
            let handle = provide_tour();
            let open = json!({ "palette": true });
            let started =
                use_hook(|| handle.start_with_gate(parse_gate(&open, StartSource::Elsewhere)));
            let toasts = handle.toasts.read();
            let said = toasts
                .iter()
                .map(|t| t.message.clone())
                .collect::<Vec<_>>()
                .join("|");
            rsx! { p { "started={started} active={handle.is_active()} said={said}" } }
        }
        fn from_palette() -> Element {
            let handle = provide_tour();
            let open = json!({ "palette": true });
            let started =
                use_hook(|| handle.start_with_gate(parse_gate(&open, StartSource::Palette)));
            rsx! { p { "started={started} active={handle.is_active()}" } }
        }
        let html = render(from_help);
        assert!(html.contains("started=false active=false"), "{html}");
        assert!(html.contains("command palette first"), "{html}");
        let html = render(from_palette);
        assert!(html.contains("started=true active=true"), "{html}");
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
