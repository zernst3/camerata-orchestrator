//! Mocked data for the consumer prototype. No engine wiring — every value here
//! stands in for something the typed `IntakeForm` / `LeadEngineer` / `FleetCoordinator`
//! will eventually produce. The example app throughout is a small, relatable one
//! ("Pottery Studio") so the flow reads as a finished consumer product, not a demo.

/// A plain-language field type, the way the intake form offers it to a person who
/// cannot code. These map later to real column types; here they are just labels.
#[derive(Clone, Copy, PartialEq)]
pub struct FieldType {
    pub label: &'static str,
    /// A tiny glyph so the type is scannable without reading.
    pub glyph: &'static str,
}

pub const FIELD_TYPES: &[FieldType] = &[
    FieldType {
        label: "text",
        glyph: "Aa",
    },
    FieldType {
        label: "a number",
        glyph: "12",
    },
    FieldType {
        label: "a price",
        glyph: "$",
    },
    FieldType {
        label: "a date",
        glyph: "◷",
    },
    FieldType {
        label: "yes / no",
        glyph: "◐",
    },
    FieldType {
        label: "a link to another thing",
        glyph: "⇄",
    },
];

/// One field on an entity, as the user types it.
#[derive(Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub type_label: String,
}

/// One entity, with its fields and the consumer-word features (CRUD-ish).
#[derive(Clone, PartialEq)]
pub struct Entity {
    pub name: String,
    pub fields: Vec<Field>,
    /// Per-entity features in consumer words: "add", "see a list", "edit", …
    pub features: Vec<String>,
}

/// One role and its top actions ("As a [role], I want to [action].").
#[derive(Clone, PartialEq)]
pub struct Role {
    pub name: String,
    pub actions: Vec<String>,
}

/// A pre-filled intake, so the prototype opens on a believable, finished-looking
/// form rather than empty boxes. A recorder can edit, or just hit submit.
pub fn seed_entities() -> Vec<Entity> {
    vec![
        Entity {
            name: "Class".into(),
            fields: vec![
                Field {
                    name: "Title".into(),
                    type_label: "text".into(),
                },
                Field {
                    name: "Day & time".into(),
                    type_label: "a date".into(),
                },
                Field {
                    name: "Price".into(),
                    type_label: "a price".into(),
                },
                Field {
                    name: "Seats".into(),
                    type_label: "a number".into(),
                },
            ],
            features: vec![
                "add".into(),
                "see a list".into(),
                "edit".into(),
                "remove".into(),
            ],
        },
        Entity {
            name: "Student".into(),
            fields: vec![
                Field {
                    name: "Name".into(),
                    type_label: "text".into(),
                },
                Field {
                    name: "Email".into(),
                    type_label: "text".into(),
                },
                Field {
                    name: "Member?".into(),
                    type_label: "yes / no".into(),
                },
            ],
            features: vec![
                "add".into(),
                "see a list".into(),
                "edit".into(),
                "search".into(),
            ],
        },
        Entity {
            name: "Booking".into(),
            fields: vec![
                Field {
                    name: "Which class".into(),
                    type_label: "a link to another thing".into(),
                },
                Field {
                    name: "Which student".into(),
                    type_label: "a link to another thing".into(),
                },
                Field {
                    name: "Paid?".into(),
                    type_label: "yes / no".into(),
                },
            ],
            features: vec!["add".into(), "see a list".into(), "remove".into()],
        },
    ]
}

pub fn seed_roles() -> Vec<Role> {
    vec![
        Role {
            name: "Studio owner".into(),
            actions: vec![
                "set up classes and prices".into(),
                "see who has booked".into(),
            ],
        },
        Role {
            name: "Student".into(),
            actions: vec!["browse classes".into(), "book a seat".into()],
        },
    ]
}

pub const SEED_APP_NAME: &str = "Riverside Pottery Studio";

pub const SEED_DESCRIPTION: &str =
    "A small site for my pottery studio so students can see the weekly classes and \
     book a seat, and I can keep track of who's coming and who's paid.";

pub const SEED_CONSTRAINTS: &str =
    "Should feel warm and handmade, not corporate. A class shouldn't be bookable \
     past its number of seats. I'd like it to work nicely on a phone.";

// ---------------------------------------------------------------------------
// Clarify screen — the hero. A short, mocked conversation with the lead engineer.
// ---------------------------------------------------------------------------

/// What kind of clarify turn this is, so the hero screen can give the lead
/// engineer's *suggestion* a distinct, warmer treatment than a plain question.
#[derive(Clone, Copy, PartialEq)]
pub enum TurnKind {
    /// A gap or contradiction the engineer needs the user to settle.
    Question,
    /// A product-level idea the engineer proactively raises — the thing a good
    /// Product Owner would have missed. This is the engineer earning its title.
    Suggestion,
}

/// A single turn the lead engineer drives: a focused question (or a suggestion),
/// the short reason it matters, the quick-reply chips offered, and the answer the
/// user "gives" when a chip is tapped. Each answered turn nudges the confidence
/// score up by `confidence_gain`.
#[derive(Clone, PartialEq)]
pub struct ClarifyTurn {
    pub kind: TurnKind,
    pub question: String,
    pub reason: String,
    pub chips: Vec<String>,
    /// The answer that lands in the transcript when this turn is accepted. The
    /// user can also free-type, but tapping a chip uses this for a clean demo.
    pub answer: String,
    /// How many points the confidence score climbs once this turn is answered.
    pub confidence_gain: u8,
}

/// Where the confidence score starts, the moment the engineer has read the form
/// but pinned nothing down yet. Honest: a vague-but-complete ticket is workable,
/// not certain.
pub const CONFIDENCE_START: u8 = 58;

pub fn clarify_turns() -> Vec<ClarifyTurn> {
    vec![
        ClarifyTurn {
            kind: TurnKind::Question,
            question: "When a class fills up, should a student still be able to join a waitlist, or is it simply full?".into(),
            reason: "It changes whether a booking can exist without a seat, which I need to settle before building the data model.".into(),
            chips: vec!["Just full".into(), "Offer a waitlist".into()],
            answer: "Offer a waitlist".into(),
            confidence_gain: 9,
        },
        ClarifyTurn {
            kind: TurnKind::Question,
            question: "If a student cancels a booking, should their seat free up automatically for someone else?".into(),
            reason: "I want the booking list to stay honest without you having to tidy it by hand.".into(),
            chips: vec!["Yes, free it up".into(), "Hold it for me to confirm".into()],
            answer: "Yes, free it up".into(),
            confidence_gain: 8,
        },
        ClarifyTurn {
            kind: TurnKind::Question,
            question: "Who should be able to see a student's email — just you, or other students too?".into(),
            reason: "This is a privacy line I'd rather get right now than fix after launch.".into(),
            chips: vec!["Only me".into(), "Everyone".into()],
            answer: "Only me".into(),
            confidence_gain: 8,
        },
        // The HERO beat: a product-level need the user never thought of, in plain
        // language. The engineer earning its title.
        ClarifyTurn {
            kind: TurnKind::Suggestion,
            question: "One thing you didn't ask for, but I'd suggest: a simple admin area for you.".into(),
            reason: "You mentioned you'll log in to manage classes. Sites like this usually also need a private place where you can manage people and decide who's allowed to do what — so a helper could take bookings without seeing everything. I can include a small users-and-permissions area. Want it?".into(),
            chips: vec!["Yes, add that".into(), "Not now".into()],
            answer: "Yes, add that".into(),
            confidence_gain: 12,
        },
        ClarifyTurn {
            kind: TurnKind::Question,
            question: "Last one: should past classes drop off the list on their own, or stay visible as history?".into(),
            reason: "It decides whether the class list shows time, or everything ever.".into(),
            chips: vec!["Hide past ones".into(), "Keep as history".into()],
            answer: "Keep as history".into(),
            confidence_gain: 5,
        },
    ]
}

/// The opening line the engineer says before the first question, so the hero
/// screen doesn't begin cold. It sets the tone: read the whole thing, a few gaps.
pub const CLARIFY_OPENER: &str =
    "I've read your whole brief — this is a clear, well-scoped idea. Before I build, \
     I have a few things to pin down so I get it right the first time. Quick ones.";

/// The line the engineer says once the checklist is complete and the plan is ready.
pub const CLARIFY_READY: &str =
    "That's everything I needed. I'm confident I can build this well. Here's the plan \
     — have a look, and build it when you're happy.";

/// One node in the approved plan's visual map: an entity and what a person can
/// do with it, plus the human decision that shaped it (folded back from clarify).
#[derive(Clone, PartialEq)]
pub struct PlanNode {
    pub entity: String,
    pub actions: Vec<String>,
    pub note: Option<String>,
}

pub fn plan_map() -> Vec<PlanNode> {
    vec![
        PlanNode {
            entity: "Class".into(),
            actions: vec!["add".into(), "list".into(), "edit".into(), "remove".into()],
            note: Some("past classes kept as history".into()),
        },
        PlanNode {
            entity: "Student".into(),
            actions: vec!["add".into(), "list".into(), "edit".into(), "search".into()],
            note: Some("email visible only to you".into()),
        },
        PlanNode {
            entity: "Booking".into(),
            actions: vec!["add".into(), "list".into(), "cancel".into()],
            note: Some("waitlist when full · seat frees on cancel".into()),
        },
        // Folded in from the accepted product-level suggestion in clarify.
        PlanNode {
            entity: "People & permissions".into(),
            actions: vec!["invite".into(), "set role".into(), "remove".into()],
            note: Some("the admin area I suggested".into()),
        },
    ]
}

/// The plain-language plan prose, in the user's own words, that they approve.
pub const PLAN_PROSE: &str =
    "Here's what I'll build for you: a warm, phone-friendly site for Riverside \
     Pottery Studio. People can browse the weekly classes and book a seat; when a \
     class is full they can join a waitlist, and a cancelled seat opens up on its \
     own. You'll have a private place to add classes and prices, see your students, \
     and check who's booked and who's paid — plus a small admin area to invite \
     helpers and decide who can do what. Student emails stay visible only to you, \
     and past classes stick around as history.";

// ---------------------------------------------------------------------------
// Build screen — calm progress narrative. Human-readable stages only.
// ---------------------------------------------------------------------------

/// A build stage as the user reads it. No logs, no percentages — just a verb and
/// an object, completing one by one with a quiet check.
#[derive(Clone, PartialEq)]
pub struct BuildStage {
    pub label: &'static str,
    /// Roughly how long this stage dwells before completing, in milliseconds.
    /// Hand-tuned so the narrative breathes rather than races.
    pub dwell_ms: u64,
}

pub const BUILD_STAGES: &[BuildStage] = &[
    BuildStage {
        label: "Setting up the project",
        dwell_ms: 1100,
    },
    BuildStage {
        label: "Building the data model",
        dwell_ms: 1600,
    },
    BuildStage {
        label: "Creating the screens",
        dwell_ms: 1900,
    },
    BuildStage {
        label: "Wiring up booking and the waitlist",
        dwell_ms: 1700,
    },
    BuildStage {
        label: "Adding your admin area",
        dwell_ms: 1500,
    },
    BuildStage {
        label: "Checking it against the rules",
        dwell_ms: 2100,
    },
    BuildStage {
        label: "Putting it together for you to try",
        dwell_ms: 1400,
    },
];

/// A genuine mid-build question the lead engineer surfaces *once*, calmly, instead
/// of guessing. It appears after a specific stage and the build quietly waits on
/// the user's answer — never an error, just the engineer still listening.
#[derive(Clone, PartialEq)]
pub struct MidBuildQuestion {
    /// The stage index (0-based, into BUILD_STAGES) after which the question
    /// surfaces. The build pauses here until the user answers.
    pub after_stage: usize,
    pub question: String,
    pub reason: String,
    pub chips: Vec<String>,
}

pub fn mid_build_question() -> MidBuildQuestion {
    MidBuildQuestion {
        // Surfaces while creating the screens — a real fork that changes the build.
        after_stage: 2,
        question: "Quick one while I build the booking list: when someone books, should they get a confirmation email, or is seeing it in the list enough?".into(),
        reason: "Either is easy, but it's the kind of thing that's awkward to add later, so I'd rather ask now than guess.".into(),
        chips: vec!["Send a confirmation".into(), "The list is enough".into()],
    }
}

// ---------------------------------------------------------------------------
// QA screen — the user tests their own DRAFT app. A mocked preview of the
// generated app the user clicks around, plus the honest "is this what you meant?"
// ---------------------------------------------------------------------------

/// One row in the mocked preview of the generated app's class list, so the QA
/// screen shows a believable working app rather than a placeholder rectangle.
#[derive(Clone, PartialEq)]
pub struct PreviewClass {
    pub title: String,
    pub when: String,
    pub price: String,
    /// e.g. "5 of 8 booked" — human, not a raw count.
    pub seats: String,
    /// True once the class is full, so the preview can show the waitlist state
    /// the user asked for and confirm the rule actually landed.
    pub full: bool,
}

pub fn preview_classes() -> Vec<PreviewClass> {
    vec![
        PreviewClass {
            title: "Wheel throwing — beginners".into(),
            when: "Tue 6:30pm".into(),
            price: "$45".into(),
            seats: "5 of 8 booked".into(),
            full: false,
        },
        PreviewClass {
            title: "Hand-building bowls".into(),
            when: "Thu 7:00pm".into(),
            price: "$40".into(),
            seats: "8 of 8 booked".into(),
            full: true,
        },
        PreviewClass {
            title: "Glazing workshop".into(),
            when: "Sat 10:00am".into(),
            price: "$55".into(),
            seats: "3 of 10 booked".into(),
            full: false,
        },
    ]
}

/// The things the user asked for, restated as checkable claims, so QA is honest:
/// "here's what you wanted — does it actually do each one?" These are folded back
/// from the intake + clarify decisions.
pub fn qa_checklist() -> Vec<&'static str> {
    vec![
        "Browse the weekly classes and book a seat",
        "A full class offers a waitlist instead of blocking",
        "A cancelled seat frees up on its own",
        "Only you can see a student's email",
        "Past classes stay visible as history",
        "An admin area to invite helpers and set who can do what",
    ]
}

/// The honest framing line at the top of QA: this is a draft you verify, not a
/// finished thing dropped on you.
pub const QA_INTRO: &str =
    "Here's your app, in draft. Click around and try the things you asked for. \
     Nothing's live yet — you're the one who decides when it's ready.";

// ---------------------------------------------------------------------------
// Bug form — the strict, structured problem report (what I did / expected /
// happened / where). Like intake, strict in shape so the agents can act on it.
// ---------------------------------------------------------------------------

/// One field on the structured bug report. Strict shape: every report forces the
/// same four things, so a vague "it's broken" can't get through.
#[derive(Clone, Copy, PartialEq)]
pub struct BugField {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub placeholder: &'static str,
}

pub const BUG_FIELDS: &[BugField] = &[
    BugField {
        key: "where",
        label: "Where did it happen?",
        hint: "Which screen or feature were you on?",
        placeholder: "e.g. the class list, on my phone",
    },
    BugField {
        key: "did",
        label: "What did you do?",
        hint: "The steps you took, as best you remember.",
        placeholder: "e.g. I tapped Book on the Thursday class",
    },
    BugField {
        key: "expected",
        label: "What did you expect to happen?",
        hint: "What you thought you'd see.",
        placeholder: "e.g. it would add me to the waitlist",
    },
    BugField {
        key: "happened",
        label: "What actually happened?",
        hint: "What you saw instead.",
        placeholder: "e.g. nothing happened when I tapped it",
    },
];

/// The calm stages the structured bug runs through when sent back into the
/// governed build loop — same shape as a build, in miniature.
pub const FIX_STAGES: &[BuildStage] = &[
    BuildStage {
        label: "Reading your report",
        dwell_ms: 1100,
    },
    BuildStage {
        label: "Finding the cause",
        dwell_ms: 1700,
    },
    BuildStage {
        label: "Making the fix",
        dwell_ms: 1600,
    },
    BuildStage {
        label: "Checking it against the rules",
        dwell_ms: 1800,
    },
];

// ---------------------------------------------------------------------------
// Live screen — the payoff.
// ---------------------------------------------------------------------------

/// The (mocked) URL the app deploys to, on the user's own cloud.
pub const LIVE_URL: &str = "riverside-pottery.azurewebsites.net";
