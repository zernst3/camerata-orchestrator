//! The inspection walkthrough (V1 item 25; `docs/design/2026-10-27_inspection-walkthrough.md`), as
//! pure logic.
//!
//! A guided mode: a small popup points at the real control on screen, says in a sentence or two
//! what it does, and moves on as the user goes. Everything that decides anything lives here, with
//! no window:
//!
//! - [`STEPS`]: the ONE step table (id, `data-tour` target, title, body, "how to get there",
//!   advance trigger). Wording is edited in this one place and passes the same text checks (no
//!   em-dashes, at most two sentences) as the rest of the UI strings.
//! - [`target`]: the `data-tour` ids. The Dioxus side puts these on the real controls and the step
//!   table points at them through the same constants, so a control that loses its id, or a step
//!   that names an id no control carries, is caught by a test (see `cockpit/tour_targets.rs` in
//!   `camerata-ui`), not by a stranded popup.
//! - [`Tour`]: which step is current, and what moves it (Next, Back, Exit, a click on a control, a
//!   control appearing).
//! - [`popup_mode`], [`tour_gate`] and [`place`]: when the popup shows, when it yields, and where it
//!   sits.
//! - [`TourPrefs`] and [`should_offer`]: the remembered switch and the one-time first-launch offer.
//!
//! The walkthrough never clicks, types or decides anything. It only watches (which controls are on
//! screen, which one was clicked) and says what it sees.

use std::collections::BTreeSet;

// ── The target ids ───────────────────────────────────────────────────────────────────────

/// The `data-tour` ids. One constant per control a step points at (and the one control a step
/// waits to appear). The control carries `data-tour="<id>"`; the step table names the same
/// constant.
pub mod target {
    /// The "Create a project" block on the projects home.
    pub const PROJECT_NEW: &str = "project-new";
    /// The "Onboard repos" tab, present only inside an open project.
    pub const NAV_ONBOARD: &str = "nav-onboard";
    /// The button that browses for the client repository.
    pub const REPO_ADD: &str = "repo-add";
    /// The client name field.
    pub const CLIENT_NAME: &str = "client-name";
    /// The Settings tab (credentials and the model backend live behind it).
    pub const SETTINGS: &str = "settings";
    /// The proposed stack rules.
    pub const RULES: &str = "rules";
    /// The pre-run cost estimate.
    pub const COST_ESTIMATE: &str = "cost-estimate";
    /// The button that starts the inspection.
    pub const RUN_INSPECTION: &str = "run-inspection";
    /// The warning shown when a pass failed or did not run.
    pub const FAILED_PASSES: &str = "failed-passes";
    /// The review queue (the findings toolbar and table).
    pub const REVIEW_QUEUE: &str = "review-queue";
    /// The Commands button: the door to the palette, and the place the stacked view and the code
    /// viewer are reached from.
    pub const SPEED_TOOLS: &str = "speed-tools";
    /// The Review time button.
    pub const REVIEW_TIME: &str = "review-time";
    /// The button that exports the package.
    pub const EXPORT_PACKAGE: &str = "export-package";
}

/// The attribute the ids ride on.
pub const TOUR_ATTRIBUTE: &str = "data-tour";

// ── The step table ───────────────────────────────────────────────────────────────────────

/// What moves a step along without the user pressing Next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    /// Nothing: the step only explains, and Next moves on.
    Next,
    /// The user clicked the step's own control.
    Click,
    /// This control is on screen: it appeared (it was not there a moment ago), or it was already
    /// there at the first look after the tour started. Either way the user has done the thing.
    Appears(&'static str),
}

/// Where the keyboard goes when the walkthrough closes on a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitFocus {
    /// Back on the highlighted control itself (when it takes focus), as the user left it.
    Control,
    /// On the block around the control, never on the control. For a control whose activation
    /// starts a paid run, an export, a deletion or any other write: a reflex Enter after closing
    /// must not trigger it.
    Block,
}

/// The screen a step's control lives on. It exists so a test can render each screen and check
/// the control is there; nothing at run time reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Screen {
    /// The projects home.
    Projects,
    /// The top bar inside an open project.
    ProjectNav,
    /// The Onboard repos tab before a scan.
    Onboard,
    /// The scan result: proposed rules, the audit controls, the Queue, the export.
    Scan,
    /// The warning banner for an inspection whose AI review did not run.
    FailedBanner,
}

/// One step of the tour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// A stable name for the step.
    pub id: &'static str,
    /// The `data-tour` id of the control it points at.
    pub target: &'static str,
    /// Where that control lives.
    pub screen: Screen,
    /// A few words.
    pub title: &'static str,
    /// One or two plain sentences.
    pub body: &'static str,
    /// What to say when the control is not on screen: how to reach it.
    pub how_to_get_there: &'static str,
    /// What moves the step on by itself.
    pub advance: Advance,
    /// Where the keyboard goes when the walkthrough closes on this step.
    pub focus_on_exit: ExitFocus,
}

/// THE STEP TABLE: the inspection, from an empty app to an exported package. Twelve steps.
///
/// `focus_on_exit` is [`ExitFocus::Block`] for the steps whose control starts a paid run or an
/// export (Run, Export): closing the walkthrough there leaves the keyboard on the block around
/// the button, so a reflex Enter cannot start it.
///
/// Where a step's own control does not exist yet, it points at the closest one that does (the
/// spending cap is described beside the cost estimate). Re-point the `target` here when the real
/// control lands.
pub const STEPS: [Step; 12] = [
    Step {
        id: "project",
        target: target::PROJECT_NEW,
        screen: Screen::Projects,
        title: "Create or open a project",
        body: "A project holds one client's repository, rules and results. Create one here, or open one you already have.",
        how_to_get_there: "Press Projects in the top bar, or the Back to Projects arrow inside a project.",
        advance: Advance::Appears(target::NAV_ONBOARD),
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "repository",
        target: target::REPO_ADD,
        screen: Screen::Onboard,
        title: "Add the client repository",
        body: "Browse to the local copy of the client's repository, which Camerata only reads. That copy is deleted after delivery.",
        how_to_get_there: "Open the project, press Onboard repos in the top bar, and choose Brownfield.",
        advance: Advance::Click,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "client",
        target: target::CLIENT_NAME,
        screen: Screen::Scan,
        title: "Client name and setup",
        body: "The client name fills the confidentiality line on every page of the report. Export stays blocked until it is set.",
        how_to_get_there: "Scan the repository first. The client name field sits above the cost estimate.",
        advance: Advance::Next,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "backend",
        target: target::SETTINGS,
        screen: Screen::ProjectNav,
        title: "Model backend",
        body: "Use the API for client work, with the key saved in your Keychain. Open Settings, then Chat backend, to check it.",
        how_to_get_there: "Open a project to see the Settings tab in the top bar.",
        advance: Advance::Click,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "rules",
        target: target::RULES,
        screen: Screen::Scan,
        title: "Stack rules",
        body: "These rules were picked for the stack Camerata found, and each one says why. Leave the defaults unless you have a reason.",
        how_to_get_there: "Open Onboard repos and scan the repository to see the proposed rules.",
        advance: Advance::Next,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "estimate",
        target: target::COST_ESTIMATE,
        screen: Screen::Scan,
        title: "Count and estimate",
        body: "The line count is what the quote is priced on, and the estimate is the model cost of this run. Check both against your spending cap before you run.",
        how_to_get_there: "Scan the repository first. The estimate appears above the run button.",
        advance: Advance::Next,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "run",
        target: target::RUN_INSPECTION,
        screen: Screen::Scan,
        title: "Run the inspection",
        body: "Press the highlighted button to start. The progress view shows each pass, and the run can go unattended.",
        how_to_get_there: "Open Onboard repos and scan the repository. The run button is under the proposed rules.",
        advance: Advance::Click,
        focus_on_exit: ExitFocus::Block,
    },
    Step {
        id: "failed",
        target: target::FAILED_PASSES,
        screen: Screen::FailedBanner,
        title: "Failed or skipped passes",
        body: "A warning here means a pass failed or did not run. The export says so too, so nothing is hidden.",
        how_to_get_there: "This warning shows under the results only when a pass failed or was skipped.",
        advance: Advance::Next,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "queue",
        target: target::REVIEW_QUEUE,
        screen: Screen::Scan,
        title: "The review queue",
        body: "Decide each finding with C to confirm, I to ignore, T for tech debt or F for a false positive. Ignore and false positive ask for a reason, and Undo lasts five seconds.",
        how_to_get_there: "Run the inspection. The review queue appears under the results.",
        advance: Advance::Next,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "speed",
        target: target::SPEED_TOOLS,
        screen: Screen::Scan,
        title: "Speed tools",
        body: "Stack a whole group to decide it at once, open the code viewer, or press Cmd+K for every command in one list. This button opens that list.",
        how_to_get_there: "Run the inspection. The Commands button is in the toolbar above the queue.",
        advance: Advance::Click,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "time",
        target: target::REVIEW_TIME,
        screen: Screen::Scan,
        title: "Review time and counts",
        body: "Your review time and counts are for you only. They never appear in the export.",
        how_to_get_there: "Run the inspection. The Review time button is in the progress line above the queue.",
        advance: Advance::Click,
        focus_on_exit: ExitFocus::Control,
    },
    Step {
        id: "export",
        target: target::EXPORT_PACKAGE,
        screen: Screen::Scan,
        title: "Export the package",
        body: "The ZIP holds the PDF report, the Excel workbook and the JSON, with a README that explains each file. The release checks run first.",
        how_to_get_there: "Run the inspection. The export is below the review queue.",
        advance: Advance::Click,
        focus_on_exit: ExitFocus::Block,
    },
];

/// Every `data-tour` id the walkthrough depends on: each step's target, and each control a step
/// waits for.
pub fn required_ids() -> BTreeSet<&'static str> {
    let mut out = BTreeSet::new();
    for step in &STEPS {
        out.insert(step.target);
        if let Advance::Appears(id) = step.advance {
            out.insert(id);
        }
    }
    out
}

/// How many sentences a text holds: a `.`, `!` or `?` that ends the text or is followed by a
/// space. (A dot inside `Cmd+K` or a number does not end one.)
pub fn sentence_count(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut count = 0;
    for (i, c) in chars.iter().enumerate() {
        if matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|n| n.is_whitespace()) {
            count += 1;
        }
    }
    count
}

// ── The tour ─────────────────────────────────────────────────────────────────────────────

/// What a step change did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Moved to another step.
    Moved,
    /// Went past the last step: the tour is over.
    Finished,
    /// Nothing to do (the tour is off, or Back on the first step).
    Idle,
}

/// Where the tour is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tour {
    active: bool,
    index: usize,
    /// The controls on screen at the last look, so that "appeared" means "was not there before".
    /// `None` until the first look after the tour started; on that look a control that is already
    /// there satisfies an `Advance::Appears` step (see [`Tour::observe`]).
    seen: Option<BTreeSet<String>>,
}

impl Tour {
    /// A tour that is off.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the tour is running.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Start (or restart) at the first step.
    pub fn start(&mut self) {
        self.active = true;
        self.index = 0;
        self.seen = None;
    }

    /// Stop.
    pub fn exit(&mut self) {
        self.active = false;
        self.seen = None;
    }

    /// The current step, or `None` when the tour is off.
    pub fn step(&self) -> Option<&'static Step> {
        self.active.then(|| STEPS.get(self.index)).flatten()
    }

    /// "Step N of M" as `(N, M)`, or `None` when the tour is off.
    pub fn position(&self) -> Option<(usize, usize)> {
        self.active.then_some((self.index + 1, STEPS.len()))
    }

    /// The label of the forward button: "Next", or "Done" on the last step.
    pub fn next_label(&self) -> &'static str {
        if self.index + 1 >= STEPS.len() {
            "Done"
        } else {
            "Next"
        }
    }

    /// Whether Back does anything.
    pub fn can_back(&self) -> bool {
        self.active && self.index > 0
    }

    /// The Next button. On the last step it ends the tour.
    pub fn go_next(&mut self) -> Progress {
        if !self.active {
            return Progress::Idle;
        }
        if self.index + 1 >= STEPS.len() {
            self.exit();
            return Progress::Finished;
        }
        self.index += 1;
        Progress::Moved
    }

    /// The Back button.
    pub fn back(&mut self) -> Progress {
        if !self.can_back() {
            return Progress::Idle;
        }
        self.index -= 1;
        Progress::Moved
    }

    /// The user clicked the control carrying `id`. Advances when it is the current step's click
    /// trigger. Answers whether the tour moved on (or finished).
    pub fn click(&mut self, id: &str) -> bool {
        let Some(step) = self.step() else {
            return false;
        };
        if step.advance == Advance::Click && step.target == id {
            return self.go_next() != Progress::Idle;
        }
        false
    }

    /// A look at the screen: `present` is every `data-tour` id on it now. Advances when the
    /// current step waits for a control (`Advance::Appears`) and that control is satisfied:
    ///
    /// - it was not on screen at the last look and is now (the user did the thing), or
    /// - this is the FIRST look since the tour started and it is already there (the tour was
    ///   started with the thing already done, for example from inside an open project, so the
    ///   step has nothing left to wait for).
    ///
    /// Only that first look counts a control that is already there. After it, a control that
    /// stays on screen is not an appearance, so going Back to such a step does not skip it again.
    /// Answers whether the tour moved on.
    pub fn observe(&mut self, present: &BTreeSet<String>) -> bool {
        let Some(step) = self.step() else {
            return false;
        };
        let satisfied = match step.advance {
            Advance::Appears(id) => match &self.seen {
                Some(before) => present.contains(id) && !before.contains(id),
                None => present.contains(id),
            },
            _ => false,
        };
        self.seen = Some(present.clone());
        if satisfied {
            return self.go_next() != Progress::Idle;
        }
        false
    }
}

// ── When the popup shows ─────────────────────────────────────────────────────────────────

/// Whether something owns the screen the popup would sit on, and if so what.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TourGate {
    /// Nothing does: the popup may show.
    Open,
    /// A reason prompt is open and owns the keyboard.
    ReasonPrompt,
    /// The code viewer, the stacked view or the palette is open over the Queue.
    Overlay,
    /// Some other dialog is open.
    Modal,
}

/// THE WALKTHROUGH'S GATE, as a pure function of what is open: the same shape as the palette's.
/// A reason prompt is named before an overlay, so the reviewer is told about the one with
/// something half-written in it. Used to hide the popup while any of these is up, and to refuse
/// starting the walkthrough from a door (the Help menu, the palette) while one is.
pub fn tour_gate(reason_prompt_open: bool, overlay_open: bool, modal_open: bool) -> TourGate {
    if reason_prompt_open {
        TourGate::ReasonPrompt
    } else if overlay_open {
        TourGate::Overlay
    } else if modal_open {
        TourGate::Modal
    } else {
        TourGate::Open
    }
}

impl TourGate {
    /// Whether the popup may show.
    pub fn is_open(self) -> bool {
        self == Self::Open
    }

    /// What to tell the user when the walkthrough was asked for and refused. `None` when open.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Open => None,
            Self::ReasonPrompt => {
                Some("Finish or cancel the reason first, then start the walkthrough.")
            }
            Self::Overlay => Some(
                "Close the code viewer, the stacked view or the command palette first, then start the walkthrough.",
            ),
            Self::Modal => Some("Close the open dialog first, then start the walkthrough."),
        }
    }
}

/// Where a request to start the walkthrough came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartSource {
    /// The command palette's own "Start walkthrough" line. The palette is open (it is running the
    /// line) and closes itself, so its overlay is not a reason to refuse.
    Palette,
    /// Anything else: the Help menu, the switch, the first-launch offer. An open palette is an
    /// overlay like any other, and starting under it would put the popup behind a layer that
    /// still owns the keyboard.
    Elsewhere,
}

/// The gate for STARTING the walkthrough, from what the page holds. `viewer_or_stack` is the
/// code viewer or the stacked view; `palette` is the command palette. Only the palette's own line
/// may start it with the palette open.
pub fn start_gate(
    reason_prompt: bool,
    viewer_or_stack: bool,
    palette: bool,
    modal: bool,
    source: StartSource,
) -> TourGate {
    let palette_blocks = palette && source != StartSource::Palette;
    tour_gate(reason_prompt, viewer_or_stack || palette_blocks, modal)
}

/// What the popup does right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupMode {
    /// Not drawn: the tour is off, or something owns the screen.
    Hidden,
    /// The control is not on screen: say how to get there instead of pointing at nothing.
    Directions,
    /// Anchored to the control, with an arrow and a highlight.
    Pointing,
}

/// The popup's mode from the three facts that decide it.
pub fn popup_mode(active: bool, gate: TourGate, target_visible: bool) -> PopupMode {
    if !active || !gate.is_open() {
        PopupMode::Hidden
    } else if target_visible {
        PopupMode::Pointing
    } else {
        PopupMode::Directions
    }
}

// ── Where the popup sits ─────────────────────────────────────────────────────────────────

/// A rectangle in viewport pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The visible window, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Viewport {
    pub w: f64,
    pub h: f64,
}

/// Which side of the control the popup sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The popup is below the control; the arrow points up.
    Below,
    /// The popup is above the control; the arrow points down.
    Above,
}

/// Where to draw the popup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub side: Side,
    /// The popup's left edge.
    pub left: f64,
    /// The popup's top edge (`Below`) or the distance from the viewport's bottom to its bottom edge
    /// (`Above`).
    pub edge: f64,
    /// The arrow's centre, measured from the popup's left edge.
    pub arrow: f64,
}

/// The popup's width in pixels.
pub const POPUP_WIDTH: f64 = 340.0;
/// The room the popup is assumed to need vertically, to choose a side.
pub const POPUP_HEIGHT: f64 = 210.0;
/// The gap between the control and the popup (the arrow lives in it).
pub const POPUP_GAP: f64 = 14.0;
/// The least distance kept from the window's edge.
pub const POPUP_MARGIN: f64 = 12.0;
/// The arrow stays this far from the popup's corners.
const ARROW_INSET: f64 = 20.0;

/// Where the popup goes for a control at `target` in a window of `view`: below it when there is
/// room, otherwise above it, kept inside the window, with the arrow on the control's centre.
pub fn place(target: Rect, view: Viewport) -> Placement {
    let below_room = view.h - (target.y + target.h);
    let above_room = target.y;
    let side = if below_room >= POPUP_HEIGHT + POPUP_GAP + POPUP_MARGIN || below_room >= above_room
    {
        Side::Below
    } else {
        Side::Above
    };
    let centre = target.x + target.w / 2.0;
    let max_left = (view.w - POPUP_WIDTH - POPUP_MARGIN).max(POPUP_MARGIN);
    let left = (centre - POPUP_WIDTH / 2.0).clamp(POPUP_MARGIN, max_left);
    let arrow = (centre - left).clamp(ARROW_INSET, POPUP_WIDTH - ARROW_INSET);
    let edge = match side {
        Side::Below => (target.y + target.h + POPUP_GAP).max(POPUP_MARGIN),
        Side::Above => (view.h - target.y + POPUP_GAP).max(POPUP_MARGIN),
    };
    Placement {
        side,
        left,
        edge,
        arrow,
    }
}

// ── What is remembered ───────────────────────────────────────────────────────────────────

/// The webview storage key of the "Guided inspection" switch.
pub const KEY_GUIDED: &str = "camerata.tour.guided";
/// The webview storage key of the one-time first-launch offer.
pub const KEY_OFFER: &str = "camerata.tour.offer";

/// What happened to the one-time first-launch offer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Offer {
    /// Never shown, or shown and not answered.
    #[default]
    Pending,
    /// The user said no. Never offered again; the switch stays available.
    Declined,
    /// The user said yes. Never offered again.
    Accepted,
}

impl Offer {
    /// The stored word.
    pub fn as_word(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Declined => "declined",
            Self::Accepted => "accepted",
        }
    }

    fn from_word(word: Option<&str>) -> Self {
        match word.map(str::trim) {
            Some("declined") => Self::Declined,
            Some("accepted") => Self::Accepted,
            _ => Self::Pending,
        }
    }
}

/// The walkthrough's remembered settings. Kept per user in the app's own settings storage (the
/// webview's, like the background animation switch). They are never written to a project, so a
/// project export carries none of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TourPrefs {
    /// The "Guided inspection" switch.
    pub guided: bool,
    /// The first-launch offer.
    pub offer: Offer,
}

/// Read the settings from the two stored words (`None` when nothing is stored). Anything
/// unrecognised is the default: switch off, offer pending.
pub fn parse_prefs(guided: Option<&str>, offer: Option<&str>) -> TourPrefs {
    TourPrefs {
        guided: guided.map(str::trim) == Some("true"),
        offer: Offer::from_word(offer),
    }
}

/// The stored word for the switch.
pub fn guided_word(on: bool) -> &'static str {
    if on {
        "true"
    } else {
        "false"
    }
}

/// Whether to offer the walkthrough now: the settings have loaded, the offer has not been
/// answered, the switch is not already on, and the list of projects has loaded and is empty.
/// `projects` is `None` while the list is still loading.
pub fn should_offer(loaded: bool, prefs: TourPrefs, projects: Option<usize>) -> bool {
    loaded && prefs.offer == Offer::Pending && !prefs.guided && projects == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    // ── the table ────────────────────────────────────────────────────────────────────────

    #[test]
    fn there_are_twelve_steps_with_distinct_ids() {
        assert_eq!(STEPS.len(), 12);
        let unique: BTreeSet<_> = STEPS.iter().map(|s| s.id).collect();
        assert_eq!(unique.len(), STEPS.len());
    }

    #[test]
    fn a_control_that_starts_a_paid_run_or_an_export_never_gets_the_keyboard_back() {
        // The controls whose activation starts a paid run, an export, a deletion or any other
        // write. Add to this list when a step points at one; the table must say Block for it.
        let writes = [target::RUN_INSPECTION, target::EXPORT_PACKAGE];
        for step in &STEPS {
            let expected = if writes.contains(&step.target) {
                ExitFocus::Block
            } else {
                ExitFocus::Control
            };
            assert_eq!(step.focus_on_exit, expected, "{}", step.id);
        }
        for id in writes {
            assert!(
                STEPS.iter().any(|s| s.target == id),
                "{id} is in the list but no step points at it"
            );
        }
    }

    #[test]
    fn every_step_has_all_its_text() {
        for s in &STEPS {
            assert!(!s.title.trim().is_empty(), "{}: title", s.id);
            assert!(!s.body.trim().is_empty(), "{}: body", s.id);
            assert!(!s.how_to_get_there.trim().is_empty(), "{}: how to", s.id);
            assert!(!s.target.trim().is_empty(), "{}: target", s.id);
        }
    }

    #[test]
    fn no_step_text_has_an_em_dash_or_an_en_dash() {
        for s in &STEPS {
            for (what, text) in [
                ("title", s.title),
                ("body", s.body),
                ("how to get there", s.how_to_get_there),
            ] {
                assert!(
                    !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                    "{}: {what} has a dash: {text}",
                    s.id
                );
            }
        }
    }

    #[test]
    fn every_body_is_one_or_two_sentences() {
        for s in &STEPS {
            let n = sentence_count(s.body);
            assert!(
                (1..=2).contains(&n),
                "{}: {n} sentences in {:?}",
                s.id,
                s.body
            );
        }
    }

    #[test]
    fn the_how_to_get_there_text_is_short_too() {
        for s in &STEPS {
            let n = sentence_count(s.how_to_get_there);
            assert!(
                (1..=2).contains(&n),
                "{}: {n} sentences in {:?}",
                s.id,
                s.how_to_get_there
            );
        }
    }

    #[test]
    fn titles_are_a_few_words_and_end_without_a_full_stop() {
        for s in &STEPS {
            assert!(s.title.split_whitespace().count() <= 6, "{}", s.id);
            assert!(!s.title.ends_with('.'), "{}", s.id);
        }
    }

    #[test]
    fn sentence_count_ignores_dots_inside_words() {
        assert_eq!(sentence_count("Press Cmd+K. Then go."), 2);
        assert_eq!(sentence_count("Version 1.2 is out"), 0);
        assert_eq!(sentence_count("One. Two! Three?"), 3);
        assert_eq!(sentence_count(""), 0);
    }

    #[test]
    fn every_advance_trigger_names_something_the_tour_can_see() {
        for s in &STEPS {
            match s.advance {
                Advance::Next => {}
                Advance::Click => assert!(!s.target.is_empty()),
                Advance::Appears(id) => assert_ne!(id, s.target, "{}: waits for itself", s.id),
            }
        }
    }

    #[test]
    fn required_ids_cover_every_target_and_every_awaited_control() {
        let required = required_ids();
        for s in &STEPS {
            assert!(required.contains(s.target));
            if let Advance::Appears(id) = s.advance {
                assert!(required.contains(id));
            }
        }
    }

    // ── walking it ───────────────────────────────────────────────────────────────────────

    /// Feed the step's own advance trigger, the way the screen would.
    fn fire(tour: &mut Tour, step: &Step, last_seen: &mut BTreeSet<String>) -> bool {
        match step.advance {
            Advance::Next => tour.go_next() != Progress::Idle,
            Advance::Click => tour.click(step.target),
            Advance::Appears(id) => {
                // A look without the control, then a look with it.
                tour.observe(last_seen);
                let mut now = last_seen.clone();
                now.insert(id.to_string());
                let moved = tour.observe(&now);
                *last_seen = now;
                moved
            }
        }
    }

    #[test]
    fn walking_every_step_in_order_fires_each_trigger_and_finishes() {
        let mut tour = Tour::new();
        tour.start();
        // The first look: no project is open yet, so nothing there to satisfy step 1.
        let mut seen = BTreeSet::new();
        tour.observe(&seen);
        for (i, step) in STEPS.iter().enumerate() {
            assert_eq!(tour.step(), Some(step), "should be on step {}", i + 1);
            assert_eq!(tour.position(), Some((i + 1, STEPS.len())));
            // Before the trigger, a wrong click or a different control changes nothing.
            assert!(!tour.click("not-a-control"));
            assert!(
                fire(&mut tour, step, &mut seen),
                "{} did not advance",
                step.id
            );
        }
        assert!(!tour.is_active(), "the tour ends after the last step");
        assert_eq!(tour.step(), None);
    }

    #[test]
    fn a_click_on_another_control_does_not_advance_a_click_step() {
        let mut tour = Tour::new();
        tour.start();
        while tour.step().is_some_and(|s| s.advance != Advance::Click) {
            tour.go_next();
        }
        let step = tour.step().expect("a click step exists");
        let other = STEPS
            .iter()
            .map(|s| s.target)
            .find(|t| *t != step.target)
            .expect("another target");
        assert!(!tour.click(other));
        assert_eq!(tour.step(), Some(step));
        assert!(tour.click(step.target));
        assert_ne!(tour.step(), Some(step));
    }

    #[test]
    fn a_click_does_not_advance_a_step_that_only_explains() {
        let mut tour = Tour::new();
        tour.start();
        while tour.step().is_some_and(|s| s.advance != Advance::Next) {
            tour.go_next();
        }
        let step = tour.step().expect("an explain step exists");
        assert!(!tour.click(step.target));
        assert_eq!(tour.step(), Some(step));
    }

    fn waiting_control() -> &'static str {
        STEPS
            .iter()
            .find_map(|s| match s.advance {
                Advance::Appears(id) => Some(id),
                _ => None,
            })
            .expect("an appears step exists")
    }

    #[test]
    fn a_tour_started_with_the_thing_already_done_skips_that_step_on_the_first_look() {
        let waiting = waiting_control();
        let mut tour = Tour::new();
        tour.start();
        assert_eq!(tour.position(), Some((1, STEPS.len())));
        // Started inside an open project: the control is there from the first look.
        assert!(tour.observe(&ids(&[waiting])));
        assert_eq!(tour.position(), Some((2, STEPS.len())));
    }

    #[test]
    fn only_the_first_look_counts_a_control_that_is_already_there() {
        let waiting = waiting_control();
        let here = ids(&[waiting]);
        let mut tour = Tour::new();
        tour.start();
        // The first look finds nothing; the control then stays on screen: not an appearance.
        assert!(!tour.observe(&ids(&[])));
        assert!(!tour.observe(&ids(&[])));
        // It comes up: that is the user doing the thing.
        assert!(tour.observe(&here));
        // Going Back to the step does not skip it again while the control just sits there.
        assert_eq!(tour.back(), Progress::Moved);
        assert!(!tour.observe(&here));
        assert!(!tour.observe(&here));
        assert_eq!(tour.position(), Some((1, STEPS.len())));
        // It goes away and comes back: an appearance.
        assert!(!tour.observe(&ids(&[])));
        assert!(tour.observe(&here));
    }

    #[test]
    fn a_restart_counts_its_first_look_again() {
        let waiting = waiting_control();
        let mut tour = Tour::new();
        tour.start();
        tour.observe(&ids(&[]));
        tour.exit();
        tour.start();
        assert!(tour.observe(&ids(&[waiting])));
    }

    #[test]
    fn an_appearance_of_the_wrong_control_does_nothing() {
        let mut tour = Tour::new();
        tour.start();
        tour.observe(&ids(&[]));
        assert!(!tour.observe(&ids(&["something-else"])));
        assert_eq!(tour.position(), Some((1, STEPS.len())));
    }

    #[test]
    fn back_and_next_stay_inside_the_table() {
        let mut tour = Tour::new();
        assert_eq!(tour.go_next(), Progress::Idle, "off: nothing to move");
        assert_eq!(tour.back(), Progress::Idle);
        tour.start();
        assert!(!tour.can_back());
        assert_eq!(tour.back(), Progress::Idle);
        assert_eq!(tour.go_next(), Progress::Moved);
        assert!(tour.can_back());
        assert_eq!(tour.back(), Progress::Moved);
        assert_eq!(tour.position(), Some((1, STEPS.len())));
        assert_eq!(tour.next_label(), "Next");
        for _ in 1..STEPS.len() {
            assert_eq!(tour.go_next(), Progress::Moved);
        }
        assert_eq!(tour.next_label(), "Done");
        assert_eq!(tour.go_next(), Progress::Finished);
        assert!(!tour.is_active());
    }

    #[test]
    fn exit_turns_it_off_and_start_begins_again() {
        let mut tour = Tour::new();
        tour.start();
        tour.go_next();
        tour.exit();
        assert!(!tour.is_active());
        assert_eq!(tour.position(), None);
        tour.start();
        assert_eq!(tour.position(), Some((1, STEPS.len())));
    }

    // ── the gate and the mode ────────────────────────────────────────────────────────────

    #[test]
    fn the_gate_names_a_reason_prompt_before_an_overlay_before_a_dialog() {
        assert_eq!(tour_gate(false, false, false), TourGate::Open);
        assert_eq!(tour_gate(true, true, true), TourGate::ReasonPrompt);
        assert_eq!(tour_gate(false, true, true), TourGate::Overlay);
        assert_eq!(tour_gate(false, false, true), TourGate::Modal);
    }

    #[test]
    fn every_closed_gate_has_a_refusal_and_the_open_one_has_none() {
        assert_eq!(TourGate::Open.refusal(), None);
        for gate in [TourGate::ReasonPrompt, TourGate::Overlay, TourGate::Modal] {
            let why = gate.refusal().expect("refusal");
            assert!(!why.contains('\u{2014}'));
            assert!(why.contains("walkthrough"));
        }
    }

    #[test]
    fn the_popup_yields_to_every_closed_gate() {
        for gate in [TourGate::ReasonPrompt, TourGate::Overlay, TourGate::Modal] {
            assert_eq!(popup_mode(true, gate, true), PopupMode::Hidden);
            assert_eq!(popup_mode(true, gate, false), PopupMode::Hidden);
        }
    }

    #[test]
    fn the_popup_points_when_it_can_and_gives_directions_when_it_cannot() {
        assert_eq!(popup_mode(true, TourGate::Open, true), PopupMode::Pointing);
        assert_eq!(
            popup_mode(true, TourGate::Open, false),
            PopupMode::Directions
        );
        assert_eq!(popup_mode(false, TourGate::Open, true), PopupMode::Hidden);
    }

    #[test]
    fn only_the_palettes_own_line_may_start_it_with_the_palette_open() {
        use StartSource::{Elsewhere, Palette};
        assert_eq!(
            start_gate(false, false, true, false, Palette),
            TourGate::Open
        );
        for gate in [
            start_gate(false, false, true, false, Elsewhere),
            start_gate(false, true, true, false, Palette),
        ] {
            assert_eq!(gate, TourGate::Overlay);
        }
        // Everything else is refused for every source, in the usual order.
        for source in [Palette, Elsewhere] {
            assert_eq!(
                start_gate(true, true, true, true, source),
                TourGate::ReasonPrompt
            );
            assert_eq!(
                start_gate(false, true, false, true, source),
                TourGate::Overlay
            );
            assert_eq!(
                start_gate(false, false, false, true, source),
                TourGate::Modal
            );
            assert_eq!(
                start_gate(false, false, false, false, source),
                TourGate::Open
            );
        }
    }

    // ── placement ────────────────────────────────────────────────────────────────────────

    fn view() -> Viewport {
        Viewport {
            w: 1200.0,
            h: 800.0,
        }
    }

    #[test]
    fn a_control_near_the_top_gets_the_popup_below_with_the_arrow_on_its_centre() {
        let target = Rect {
            x: 500.0,
            y: 100.0,
            w: 100.0,
            h: 30.0,
        };
        let p = place(target, view());
        assert_eq!(p.side, Side::Below);
        assert_eq!(p.edge, 100.0 + 30.0 + POPUP_GAP);
        assert_eq!(p.left + p.arrow, 550.0, "the arrow points at the centre");
    }

    #[test]
    fn a_control_near_the_bottom_gets_the_popup_above() {
        let target = Rect {
            x: 500.0,
            y: 740.0,
            w: 100.0,
            h: 30.0,
        };
        let p = place(target, view());
        assert_eq!(p.side, Side::Above);
        assert_eq!(p.edge, 800.0 - 740.0 + POPUP_GAP);
    }

    #[test]
    fn the_popup_stays_inside_the_window_and_the_arrow_inside_the_popup() {
        let left_edge = Rect {
            x: 0.0,
            y: 100.0,
            w: 30.0,
            h: 30.0,
        };
        let p = place(left_edge, view());
        assert_eq!(p.left, POPUP_MARGIN);
        assert!(p.arrow >= 20.0 && p.arrow <= POPUP_WIDTH - 20.0);

        let right_edge = Rect {
            x: 1180.0,
            y: 100.0,
            w: 20.0,
            h: 30.0,
        };
        let p = place(right_edge, view());
        assert_eq!(p.left, 1200.0 - POPUP_WIDTH - POPUP_MARGIN);
        assert!(p.arrow >= 20.0 && p.arrow <= POPUP_WIDTH - 20.0);
    }

    #[test]
    fn a_window_narrower_than_the_popup_still_gets_a_sane_left_edge() {
        let p = place(
            Rect {
                x: 10.0,
                y: 10.0,
                w: 10.0,
                h: 10.0,
            },
            Viewport { w: 200.0, h: 600.0 },
        );
        assert_eq!(p.left, POPUP_MARGIN);
    }

    // ── what is remembered ───────────────────────────────────────────────────────────────

    #[test]
    fn nothing_stored_means_off_and_not_yet_offered() {
        let p = parse_prefs(None, None);
        assert!(!p.guided);
        assert_eq!(p.offer, Offer::Pending);
    }

    #[test]
    fn the_stored_words_round_trip() {
        for on in [true, false] {
            assert_eq!(parse_prefs(Some(guided_word(on)), None).guided, on);
        }
        for offer in [Offer::Pending, Offer::Declined, Offer::Accepted] {
            assert_eq!(parse_prefs(None, Some(offer.as_word())).offer, offer);
        }
    }

    #[test]
    fn junk_in_storage_is_the_default() {
        let p = parse_prefs(Some("maybe"), Some("???"));
        assert_eq!(p, TourPrefs::default());
    }

    #[test]
    fn the_offer_is_made_once_to_a_first_launch_with_no_projects() {
        let fresh = TourPrefs::default();
        assert!(should_offer(true, fresh, Some(0)));
        // Not before the settings and the project list have loaded.
        assert!(!should_offer(false, fresh, Some(0)));
        assert!(!should_offer(true, fresh, None));
        // Not once there is a project.
        assert!(!should_offer(true, fresh, Some(1)));
        // Not after an answer, either way.
        for offer in [Offer::Declined, Offer::Accepted] {
            let answered = TourPrefs {
                offer,
                ..TourPrefs::default()
            };
            assert!(!should_offer(true, answered, Some(0)), "{offer:?}");
        }
        // Not when the switch is already on.
        let on = TourPrefs {
            guided: true,
            ..TourPrefs::default()
        };
        assert!(!should_offer(true, on, Some(0)));
    }

    #[test]
    fn the_storage_keys_are_distinct_and_namespaced() {
        assert_ne!(KEY_GUIDED, KEY_OFFER);
        assert!(KEY_GUIDED.starts_with("camerata."));
        assert!(KEY_OFFER.starts_with("camerata."));
    }
}
