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

/// The opening line the engineer says before the first question, so the hero
/// screen doesn't begin cold. It sets the tone: read the whole thing, a few gaps.
pub const CLARIFY_OPENER: &str =
    "I've read your whole brief — this is a clear, well-scoped idea. Before I build, \
     I have a few things to pin down so I get it right the first time. Quick ones.";

/// The line the engineer says once the checklist is complete and the plan is ready.
pub const CLARIFY_READY: &str =
    "That's everything I needed. I'm confident I can build this well. Here's the plan \
     — have a look, and build it when you're happy.";

/// The plain-language plan prose, in the user's own words, that they approve.
/// Used only as a fallback when no project is present; the live PlanReveal derives
/// its prose from the real onboarding document (see `AppState::plan_prose`).
pub const PLAN_PROSE: &str =
    "Here's what I'll build for you: a clean, phone-friendly app shaped around the \
     things you track and what each person can do with them. You'll have a private \
     place to manage your data, and everything passes the same rules as it's built.";

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
        label: "Wiring up the features",
        dwell_ms: 1700,
    },
    BuildStage {
        label: "Adding the finishing touches",
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
        question: "Quick one while I build the screens: when someone adds a new record, should they get a confirmation email, or is seeing it in the list enough?".into(),
        reason: "Either is easy, but it's the kind of thing that's awkward to add later, so I'd rather ask now than guess.".into(),
        chips: vec!["Send a confirmation".into(), "The list is enough".into()],
    }
}

// ---------------------------------------------------------------------------
// QA screen — the user tests their own DRAFT app. A mocked preview of the
// generated app the user clicks around, plus the honest "is this what you meant?"
// ---------------------------------------------------------------------------

// The QA preview (the generated-app mock) and the "does it do what you asked for?"
// checklist are now derived from the REAL project in `AppState::qa_preview` /
// `qa_checklist`, so they adapt to whatever app the user described.

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
        placeholder: "e.g. the main list screen, on my phone",
    },
    BugField {
        key: "did",
        label: "What did you do?",
        hint: "The steps you took, as best you remember.",
        placeholder: "e.g. I tapped the Add button on a record",
    },
    BugField {
        key: "expected",
        label: "What did you expect to happen?",
        hint: "What you thought you'd see.",
        placeholder: "e.g. it would save and show in the list",
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
