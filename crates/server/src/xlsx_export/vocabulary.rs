//! The one vocabulary of the client package: every name a human reads in the workbook, and the
//! names the PDF shares with it.
//!
//! One name per thing, across the PDF, the workbook and every human-facing text. A word is
//! defined here, once, and the Glossary sheet prints the definition; no other file spells the
//! name for itself.
//!
//! Machine-facing files keep their own field names (`csv/`, `json/`, `findings.json`,
//! `tickets.csv`'s headers and the wire values such as `do_now` or `kind`). Those are for tools,
//! not people, and renaming them would break whatever reads them. Everything a person reads is
//! named from this module.

/// Excel refuses a sheet name over this many characters.
pub const SHEET_NAME_MAX_CHARS: usize = 31;

// ── The sheets, in reading order ────────────────────────────────────────────────────────

pub const START_HERE: &str = "Start here";
pub const GLOSSARY: &str = "Glossary";
pub const FIX_PLAN: &str = "Fix plan";
pub const ALL_PROBLEMS: &str = "All problems";
pub const NEEDS_A_DECISION: &str = "Needs a decision";
pub const YOUR_CONVENTIONS: &str = "Your conventions";
pub const DEPARTURES: &str = "Departures";
pub const DEPENDENCIES: &str = "Dependencies";
pub const RULES_CHECKED: &str = "Rules checked";
pub const DIFFERENCES_ACROSS_SERVICES: &str = "Differences across services";
pub const RULED_OUT: &str = "Ruled out";
pub const NOT_CHECKED: &str = "Not checked";
/// Excel caps a sheet name at 31 characters, so "(complete record)" is said by the sheet's own
/// first lines and by the Start here list, not by the tab.
pub const ALL_SOURCE_ROWS: &str = "All source rows";
pub const REVIEWER_DECISIONS: &str = "Reviewer decisions";
pub const HOW_PROBLEMS_WERE_GROUPED: &str = "How problems were grouped";

/// Where a sheet sits in the reading order, which sets its tab colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetGroup {
    /// Dark tabs.
    Start,
    /// Red tabs.
    WhatToFix,
    /// Gray tabs.
    WhatWeChecked,
    /// Light tabs, last.
    ForReviewers,
}

impl SheetGroup {
    /// The heading the Start here list prints over this group.
    pub fn heading(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::WhatToFix => "What to fix",
            Self::WhatWeChecked => "What we checked",
            Self::ForReviewers => "For reviewers",
        }
    }

    /// The tab colour.
    pub fn tab_color(self) -> &'static str {
        match self {
            Self::Start => "#111827",
            Self::WhatToFix => "#B91C1C",
            Self::WhatWeChecked => "#6B7280",
            Self::ForReviewers => "#D1D5DB",
        }
    }
}

/// One sheet of the workbook: its name, its group and the one line that says what it is.
#[derive(Debug, Clone, Copy)]
pub struct SheetInfo {
    pub name: &'static str,
    pub group: SheetGroup,
    pub purpose: &'static str,
}

/// Every sheet a client workbook can hold, in reading order. The Differences across services
/// sheet exists only for a project of several repos.
pub const SHEETS: [SheetInfo; 15] = [
    SheetInfo {
        name: START_HERE,
        group: SheetGroup::Start,
        purpose: "What this file is, where everything is, and the counts.",
    },
    SheetInfo {
        name: GLOSSARY,
        group: SheetGroup::Start,
        purpose: "What every word and column in this file means.",
    },
    SheetInfo {
        name: FIX_PLAN,
        group: SheetGroup::WhatToFix,
        purpose: "One row per fix, in the order to do them.",
    },
    SheetInfo {
        name: ALL_PROBLEMS,
        group: SheetGroup::WhatToFix,
        purpose: "Every problem, once, with the place it was seen and what to do there.",
    },
    SheetInfo {
        name: NEEDS_A_DECISION,
        group: SheetGroup::WhatToFix,
        purpose: "Problems a reviewer must rule on before you act on them.",
    },
    SheetInfo {
        name: YOUR_CONVENTIONS,
        group: SheetGroup::WhatToFix,
        purpose: "What your code already does consistently, and where it has no single habit.",
    },
    SheetInfo {
        name: DEPARTURES,
        group: SheetGroup::WhatToFix,
        purpose: "Where your code departs from a convention it mostly follows.",
    },
    SheetInfo {
        name: DEPENDENCIES,
        group: SheetGroup::WhatToFix,
        purpose: "Known vulnerabilities in the packages you depend on.",
    },
    SheetInfo {
        name: RULES_CHECKED,
        group: SheetGroup::WhatWeChecked,
        purpose: "Every rule we ran and what it found.",
    },
    SheetInfo {
        name: DIFFERENCES_ACROSS_SERVICES,
        group: SheetGroup::WhatWeChecked,
        purpose: "Rules your services answer differently.",
    },
    SheetInfo {
        name: RULED_OUT,
        group: SheetGroup::WhatWeChecked,
        purpose: "Looked at and not a problem, with the reason.",
    },
    SheetInfo {
        name: NOT_CHECKED,
        group: SheetGroup::WhatWeChecked,
        purpose: "What we could not or did not check.",
    },
    SheetInfo {
        name: ALL_SOURCE_ROWS,
        group: SheetGroup::ForReviewers,
        purpose: "Raw record (complete record): every row the engine produced. A problem can be \
                  listed several times here.",
    },
    SheetInfo {
        name: REVIEWER_DECISIONS,
        group: SheetGroup::ForReviewers,
        purpose: "Every decision a reviewer recorded.",
    },
    SheetInfo {
        name: HOW_PROBLEMS_WERE_GROUPED,
        group: SheetGroup::ForReviewers,
        purpose: "How problems were grouped into areas, rules and fixes.",
    },
];

/// The sheet's entry in [`SHEETS`], or `None` for a name that is not one of the client sheets.
pub fn sheet_info(name: &str) -> Option<&'static SheetInfo> {
    SHEETS.iter().find(|s| s.name == name)
}

/// The tab colour of a sheet by name; the reviewers' light tab for a name this module does not
/// know.
pub fn tab_color_of(name: &str) -> &'static str {
    sheet_info(name).map_or(SheetGroup::ForReviewers.tab_color(), |s| {
        s.group.tab_color()
    })
}

/// The questions the PDF's "Where the details are" section answers, each with the exact sheet
/// that answers it. The PDF prints these pairs; the workbook's tab names are the same constants,
/// so a name cannot differ between the two.
pub const DETAIL_MAP: [(&str, &str); 8] = [
    ("What do we fix, and in what order?", FIX_PLAN),
    ("Where exactly is each problem?", ALL_PROBLEMS),
    ("What must a reviewer rule on first?", NEEDS_A_DECISION),
    (
        "What does our code already do consistently?",
        YOUR_CONVENTIONS,
    ),
    (
        "Where does our code depart from its own habits?",
        DEPARTURES,
    ),
    ("Which packages are vulnerable?", DEPENDENCIES),
    (
        "What did we check, and what came back clean?",
        RULES_CHECKED,
    ),
    ("What did we not check?", NOT_CHECKED),
];

// ── Fix order: the one priority vocabulary ──────────────────────────────────────────────

pub const FIX_ORDER_FIRST: &str = "First";
pub const FIX_ORDER_NEXT: &str = "Next";
pub const FIX_ORDER_LATER: &str = "Later";

// ── Column headers ──────────────────────────────────────────────────────────────────────

pub const H_ORDER: &str = "Order";
pub const H_FIX_ORDER: &str = "Fix order";
pub const H_FIX: &str = "Fix";
pub const H_WHAT_IS_WRONG: &str = "What is wrong";
pub const H_WHY_IT_MATTERS: &str = "Why it matters";
pub const H_WHAT_TO_DO: &str = "What to do";
pub const H_WHAT_TO_DO_HERE: &str = "What to do here";
pub const H_EXAMPLE: &str = "Example from your code";
pub const H_SEVERITY: &str = "Severity";
pub const H_EFFORT: &str = "Effort";
pub const H_ESTIMATED_HOURS: &str = "Estimated hours";
pub const H_CONFIDENCE: &str = "Confidence";
pub const H_PLACES: &str = "Places";
pub const H_AREA: &str = "Area";
pub const H_AREAS: &str = "Areas";
pub const H_RULE: &str = "Rule";
pub const H_RULES: &str = "Rules";
pub const H_FOLDERS: &str = "Folders";
pub const H_REVIEWED_BY: &str = "Reviewed by";
pub const H_GO_TO_PLACES: &str = "Go to places";
pub const H_PROBLEM: &str = "Problem";
pub const H_FILE: &str = "File";
pub const H_LINE: &str = "Line";
pub const H_REPO: &str = "Repo";
pub const H_REVIEWED_IN_PERSON: &str = "Reviewed in person";
pub const H_NEEDS_A_DECISION: &str = "Needs a decision";
pub const H_DECISION_SO_FAR: &str = "Decision so far";
pub const H_WHY_IT_IS_WAITING: &str = "Why it is waiting";
pub const H_SOURCE: &str = "Source";
pub const H_STANDARD_CITED: &str = "Standard cited";
pub const H_CODE_SEEN: &str = "Code seen";
pub const H_FIX_TYPE: &str = "Fix type";
pub const H_ALSO_FOUND_AT: &str = "Also found at";
pub const H_SEVERITY_NOTE: &str = "Why the severity changed";
pub const H_WHAT_WE_CHECKED: &str = "What we checked";
pub const H_WHAT_YOUR_CODE_DOES: &str = "What your code does";
pub const H_HOW_CONSISTENTLY: &str = "How consistently";
pub const H_PLACES_LOOKED_AT: &str = "Places looked at";
pub const H_MORE_DETAIL: &str = "More detail";
pub const H_CONVENTION: &str = "Convention";
pub const H_PLACES_DEPARTING: &str = "Places departing";
pub const H_PLACES_CHECKED: &str = "Places checked";
pub const H_WORST_SEVERITY: &str = "Worst severity";
pub const H_WHERE: &str = "Where";
pub const H_PACKAGE: &str = "Package";
pub const H_ADVISORY: &str = "Advisory";
pub const H_FIXED_IN: &str = "Fixed in";
pub const H_RESULT: &str = "Result";
pub const H_HOW_WE_CHECKED: &str = "How we checked";
pub const H_ALTERNATIVE_APPLIED: &str = "Alternative applied";
pub const H_EVIDENCE: &str = "Evidence for it";
pub const H_GO_TO_FIX: &str = "Go to fix";
pub const H_RULED_OUT_BY: &str = "Ruled out by";
pub const H_REASON: &str = "Reason";
pub const H_GAP: &str = "Kind of gap";
pub const H_WHAT: &str = "What";
pub const H_WHY: &str = "Why";
pub const H_FOUND_HOW_MANY_WAYS: &str = "Found how many ways";
pub const H_REPRESENTED_BY: &str = "Represented by";
pub const H_ROW_ROLE: &str = "Row role";
pub const H_SHOWN_ON: &str = "Shown on";
pub const H_DECISION_ID: &str = "Decision id";
pub const H_SCOPE: &str = "Scope";
pub const H_STATE: &str = "State";
pub const H_MERGED_INTO: &str = "Merged into";
pub const H_PLACES_COVERED: &str = "Places covered";
pub const H_INHERITED_FROM: &str = "Inherited from";
pub const H_REASON_RECORDED: &str = "Reason recorded";
pub const H_REVOKED: &str = "Revoked";
pub const H_GROUP: &str = "Group";
pub const H_WITHIN: &str = "Within";
pub const H_UNDER: &str = "Under";
pub const H_LABEL: &str = "Label";
pub const H_CAUSE: &str = "Cause";
pub const H_SUMMARY: &str = "Summary";
pub const H_RULED_OUT_COUNT: &str = "Ruled out";
pub const H_REVIEWED_WITH_OTHERS: &str = "Reviewed with others like it";
pub const H_ENGINE_VERIFIED: &str = "Engine verified";
pub const H_BACK_TO_START: &str = "Back to Start here";

// Technical band (ids for tooling).
pub const H_PLACE_ID: &str = "Place id";
pub const H_FIX_ID: &str = "Fix id";
pub const H_RULE_ID: &str = "Rule id";
pub const H_RULE_IDS: &str = "Rule ids";
pub const H_KIND: &str = "Kind";
pub const H_STATUS: &str = "Status";
pub const H_CARRIED_FROM: &str = "Carried from";
pub const H_REVIEW_LEVEL: &str = "Review level";
pub const H_REVIEWER_EDITED: &str = "Reviewer edited";
pub const H_EDIT_IDS: &str = "Edit ids";
pub const H_CITATION_LINKS: &str = "Standard links";
pub const H_GROUP_ID: &str = "Group id";
pub const H_PARENT_ID: &str = "Parent group id";
/// The note on the first technical header of every sheet.
pub const TECHNICAL_BAND_NOTE: &str = "Technical (ids for tooling)";

// Columns only an internal export writes (the engine's own triage; never in a client file).
pub const H_REVIEW_FLAGS: &str = "Review flags";
pub const H_HELD: &str = "Held";
pub const H_ESCALATED: &str = "Escalated";

// The Glossary and Start here tables.
pub const H_TERM: &str = "Term";
pub const H_PLAIN_MEANING: &str = "Plain meaning";
pub const H_WHERE_YOU_MEET_IT: &str = "Where you meet it";
pub const H_SHEET: &str = "Sheet";
pub const H_WHAT_IT_IS: &str = "What it is";
pub const H_ROWS: &str = "Rows";

// Values.
pub const YES: &str = "Yes";
pub const WHOLE_FILE: &str = "whole file";
pub const ENGINE: &str = "Engine";
pub const REVIEWER: &str = "Reviewer";

/// The label for each source of a row: Camerata's own check, an outside tool's, or a model's.
pub const SOURCE_DETERMINISTIC: &str = "Deterministic";
pub const SOURCE_MODEL_REVIEWED: &str = "Model-reviewed";
/// `Tool: <name>` for a row an outside tool produced.
pub const SOURCE_TOOL_PREFIX: &str = "Tool: ";
pub const SOURCE_TOOL: &str = "Tool";

/// The Row role values of the All source rows sheet.
pub const ROLE_PRIMARY: &str = "Primary";
pub const ROLE_COLLAPSED: &str = "Collapsed source";
pub const ROLE_ALSO_FOUND: &str = "Also found";
pub const ROLE_ALSO_AFFECTED: &str = "Also affected";
pub const ROLE_ALSO_FLAGGED: &str = "Also flagged here";

/// The Not checked sheet's Kind of gap values.
pub const GAP_RULE_COULD_NOT_COMPLETE: &str = "Rule could not be completed";
pub const GAP_RULE_NOT_RUN: &str = "Rule not run";
pub const GAP_RULE_PART_OF_FILES: &str = "Rule checked part of the files";
pub const GAP_FILE_NOT_READ: &str = "File not read";
pub const GAP_TOOL_NOTE: &str = "Tool did not run";
pub const GAP_DEPENDENCY: &str = "Dependency check";
pub const GAP_REVIEW_PASS: &str = "Review pass or scope";
pub const GAP_LEFT_TO_CI: &str = "Rule left to your CI";

// ── Provenance labels (the PDF cover's words) ───────────────────────────────────────────

pub const P_REPOS_INSPECTED: &str = "Repos inspected";
pub const P_FILES_SCANNED: &str = "Files scanned";
pub const P_CODE_VOLUME: &str = "Code volume";
pub const P_GENERATED: &str = "Generated";
pub const P_WHAT_WE_CHECKED: &str = "What we checked";
pub const P_PREPARED_BY: &str = "Prepared by";
pub const P_PREPARED_FOR: &str = "Prepared for";
/// The workbook's title line.
pub const WORKBOOK_TITLE: &str = "Codebase Inspection: the details behind your report";

// ── The Glossary ────────────────────────────────────────────────────────────────────────

/// One term and what it means, with the places a reader meets it.
#[derive(Debug, Clone, Copy)]
pub struct GlossaryEntry {
    pub term: &'static str,
    pub meaning: &'static str,
    pub where_you_meet_it: &'static str,
}

/// Every term the client package uses, defined once. Sheet names and every column header used
/// on more than one sheet are here; a test holds the workbook to it.
pub const GLOSSARY_ENTRIES: &[GlossaryEntry] = &[
    // The nouns of the package.
    e(
        "Fix",
        "One cause and one change that clears a whole set of problems, wherever they appear.",
        "Fix plan; the Fix column on All problems",
    ),
    e(
        "Place",
        "One file and line, or one whole file, where a problem was seen.",
        "File and Line columns; every Places count",
    ),
    e(
        "Problem",
        "What was found at a place. A problem lists every rule it breaks.",
        "All problems; the Problem column",
    ),
    e(
        "Area",
        "The part of your system a rule belongs to, such as security or testing.",
        "Area columns; the scorecard on Start here",
    ),
    e(
        "Rule",
        "One thing we looked for.",
        "Rules checked; the Rule and Rules columns",
    ),
    e(
        "Rule id",
        "The short code that names a rule. Tools read it, and you can quote it when you ask us about a rule.",
        "The technical columns at the right of every table",
    ),
    e(
        "Fix order",
        "When to do a fix: First, Next or Later. First is every critical and high fix and the quick medium ones. Next is the rest of the medium fixes. Later is the low ones.",
        "Fix plan; All problems; Needs a decision",
    ),
    e(
        "Severity",
        "How bad the problem is if it is left alone: Critical, High, Medium or Low.",
        "Every table of problems; the scorecard on Start here",
    ),
    e(
        "Effort",
        "A rough size of the change for one place: low, medium or high.",
        "Fix plan; All problems",
    ),
    e(
        "Estimated hours",
        "The range of hours the effort stands for, for one place.",
        "All problems",
    ),
    e(
        "Confidence",
        "How sure the review is that the problem is real: High, or Low when the review was unsure. A Low problem waits on Needs a decision.",
        "All problems; Needs a decision",
    ),
    e(
        "Needs a decision",
        "A reviewer must rule on the problem before you act on it.",
        "The Needs a decision sheet; the column of that name on All problems",
    ),
    e(
        "Ruled out",
        "Looked at and not a problem. The reason is on the Ruled out sheet.",
        "The Ruled out sheet",
    ),
    e(
        "Convention",
        "A habit your code already follows consistently.",
        "Your conventions; Departures",
    ),
    e(
        "Departure",
        "A place where your code departs from a convention it mostly follows.",
        "The Departures sheet",
    ),
    e(
        "Source",
        "Who found it: Deterministic, Model-reviewed, or Tool followed by the tool's name.",
        "All problems; Needs a decision",
    ),
    e(
        "Deterministic",
        "Found by a check that cites an authoritative published standard and gives the same answer every time.",
        "The Source column; Rules checked",
    ),
    e(
        "Model-reviewed",
        "Found by a model reading your code. It may cite a related standard.",
        "The Source column; Rules checked",
    ),
    e(
        "Tool",
        "Found by an outside analysis tool that ran during the inspection.",
        "The Source column; Rules checked",
    ),
    e(
        "Related standard",
        "A published standard matched to a model-reviewed problem by topic. It is not a rule we checked.",
        "The Standard cited column",
    ),
    e(
        "Standard cited",
        "The published standard, such as an OWASP or CWE entry, behind the rule.",
        "All problems; Rules checked",
    ),
    e(
        "Reviewed in person",
        "Yes when a person read the problem in your code. Blank means the engine checked it against your files without a person reading it.",
        "All problems; Dependencies",
    ),
    e(
        "Engine verified",
        "Checked by the engine against your files, without a person reading it.",
        "How problems were grouped",
    ),
    e(
        "Example from your code",
        "A fix written for the code at one place in your repo.",
        "Fix plan",
    ),
    e(
        "Standard fix",
        "The rule's own guidance, not written for your code.",
        "The Fix type column",
    ),
    e(
        "Tailored fix",
        "A fix written for your code that passed our checks.",
        "The Fix type column",
    ),
    e(
        "Fix type",
        "Whether the fix text is a Tailored fix or a Standard fix.",
        "All problems; Departures",
    ),
    e(
        "Kind",
        "What a row is: a defect, a departure, a convention, no convention, a rule that could not be completed or a dependency advisory. Tools read it; people use the sheet it sits on.",
        "The technical columns",
    ),
    e(
        "Carried from an earlier inspection",
        "A problem carried forward unchanged because only changed files were inspected again. The column holds the earlier commit.",
        "The Carried from column",
    ),
    e(
        "Carried from",
        "The commit of the earlier inspection a problem was carried forward from. Blank when it was found this time.",
        "The technical columns of All problems",
    ),
    e(
        "Dependency advisory",
        "A published vulnerability in a package you depend on.",
        "The Dependencies sheet",
    ),
    e(
        "Status",
        "The state the engine holds a problem in, for example suppressed by an earlier baseline. For tools.",
        "The technical columns of All problems",
    ),
    e(
        "Place id",
        "A short code that names one place. It is the same in every file of this package.",
        "The technical columns",
    ),
    e(
        "Fix id",
        "A code that names one fix. It matches a row of the Fix plan.",
        "The technical columns",
    ),
    e(
        "Rule ids",
        "Every rule id behind a problem: its own rule and any rule that flagged the same place.",
        "The technical columns of All problems",
    ),
    e(
        "Review level",
        "How closely the problem was reviewed, in the engine's own words.",
        "The technical columns",
    ),
    e(
        "Merged into",
        "The Place id of the problem a reviewer merged this one into.",
        "The technical columns; Reviewer decisions",
    ),
    e(
        "Reviewer edited",
        "Yes when a reviewer changed the text of this row.",
        "The technical columns",
    ),
    e(
        "Edit ids",
        "The ids of the reviewer's edits to this row.",
        "The technical columns",
    ),
    e(
        "Standard links",
        "The web addresses of the standards cited.",
        "The technical columns of All problems",
    ),
    e(
        "Group id",
        "A code that names one group of problems: an area, a rule or a fix.",
        "The technical columns",
    ),
    e(
        "Parent group id",
        "The Group id of the group this one sits under.",
        "How problems were grouped",
    ),
    e(
        "Inherited from",
        "The id of the group decision a decision for one place was written by.",
        "Reviewer decisions",
    ),
    // The columns.
    e(
        "Order",
        "The place of a fix in the plan: 1 is the first to do.",
        "Fix plan",
    ),
    e(
        "Places",
        "How many places a fix, a problem or a group covers.",
        "Fix plan; All problems; How problems were grouped",
    ),
    e(
        "Rules",
        "The rules a fix or a problem breaks, by name.",
        "Fix plan; All problems",
    ),
    e(
        "Areas",
        "The areas a fix touches.",
        "Fix plan",
    ),
    e(
        "Folders",
        "The folders that hold most of a fix's places, with how many places each holds.",
        "Fix plan",
    ),
    e(
        "Reviewed by",
        "How closely the places of a fix were reviewed.",
        "Fix plan",
    ),
    e(
        "Go to places",
        "A link to the first place of a fix on All problems.",
        "Fix plan",
    ),
    e(
        "What is wrong",
        "The cause the places of a fix share.",
        "Fix plan",
    ),
    e(
        "Why it matters",
        "What the problem puts at risk.",
        "Fix plan",
    ),
    e(
        "What to do",
        "The change that makes the fix.",
        "Fix plan; Departures; Dependencies",
    ),
    e(
        "What to do here",
        "The change to make at this place.",
        "All problems; Needs a decision",
    ),
    e(
        "File",
        "The file a problem was seen in.",
        "Every table of problems",
    ),
    e(
        "Line",
        "The line in the file, or whole file when the problem is the file itself.",
        "Every table of problems",
    ),
    e(
        "Repo",
        "The repository a place is in.",
        "Every table of problems",
    ),
    e(
        "Code seen",
        "The lines of your code the problem was seen in.",
        "All problems",
    ),
    e(
        "Decision so far",
        "What a reviewer has already decided about the problem.",
        "All problems; Needs a decision",
    ),
    e(
        "Why it is waiting",
        "Why a reviewer must rule on the problem first.",
        "Needs a decision",
    ),
    e(
        "Also found at",
        "Other places the same problem was found, up to a limit. All of them are on All source rows.",
        "All problems; Your conventions",
    ),
    e(
        "Why the severity changed",
        "Why the severity is not the one the problem was first rated at.",
        "All problems",
    ),
    e(
        "What we checked",
        "The thing we looked for, in words.",
        "Your conventions; Differences across services",
    ),
    e(
        "What your code does",
        "The convention your code follows, or the habits we saw when it follows none.",
        "Your conventions",
    ),
    e(
        "How consistently",
        "Whether your code follows the convention everywhere we looked.",
        "Your conventions",
    ),
    e(
        "Places looked at",
        "How many places we looked at to see the convention.",
        "Your conventions",
    ),
    e(
        "More detail",
        "The longer explanation behind a row.",
        "All problems; Your conventions; Not checked",
    ),
    e(
        "Places departing",
        "How many places depart from the convention.",
        "Departures",
    ),
    e(
        "Places checked",
        "How many places we checked against the convention.",
        "Departures",
    ),
    e(
        "Worst severity",
        "The highest severity among the departing places.",
        "Departures",
    ),
    e(
        "Where",
        "The places that depart, up to a limit. All of them are on All source rows.",
        "Departures",
    ),
    e(
        "Package",
        "The software package an advisory is about.",
        "Dependencies",
    ),
    e(
        "Advisory",
        "The published description of the vulnerability.",
        "Dependencies",
    ),
    e(
        "Fixed in",
        "The first version of the package that has the patch.",
        "Dependencies",
    ),
    e(
        "Result",
        "What a rule found: how many places, or clean.",
        "Rules checked",
    ),
    e(
        "How we checked",
        "Whether a rule is Deterministic, Model-reviewed or a Tool.",
        "Rules checked",
    ),
    e(
        "Alternative applied",
        "For a rule with several accepted ways to do a thing, the way we judged your code against.",
        "Rules checked",
    ),
    e(
        "Evidence for it",
        "The file and line that showed which alternative your code uses.",
        "Rules checked",
    ),
    e(
        "Go to fix",
        "A link to the fix for the rule's problems, or to the problems when no fix covers them.",
        "Rules checked",
    ),
    e(
        "Ruled out by",
        "Who ruled the problem out: the Reviewer or the Engine.",
        "Ruled out",
    ),
    e(
        "Reason",
        "Why, in words.",
        "Ruled out; Reviewer decisions",
    ),
    e(
        "Kind of gap",
        "What sort of thing was not checked.",
        "Not checked",
    ),
    e(
        "What",
        "The rule, file or tool that was not checked.",
        "Not checked",
    ),
    e(
        "Why",
        "Why it was not checked.",
        "Not checked",
    ),
    e(
        "Row role",
        "Whether a source row is the Primary row of a problem or a row behind it: a collapsed source, or another place the same problem was found.",
        "All source rows",
    ),
    e(
        "Found how many ways",
        "How many source rows share this Place id.",
        "All source rows",
    ),
    e(
        "Represented by",
        "A link to the row that stands for this source row on the other sheets.",
        "All source rows",
    ),
    e(
        "Shown on",
        "The sheet that holds the row standing for this source row.",
        "All source rows",
    ),
    e(
        "Decision id",
        "A code that names one recorded decision.",
        "Reviewer decisions",
    ),
    e(
        "Scope",
        "Whether a decision was made on one place or on a group of places.",
        "Reviewer decisions",
    ),
    e(
        "State",
        "What the decision was: confirmed, ignored, ruled out, merged and so on.",
        "Reviewer decisions",
    ),
    e(
        "Places covered",
        "How many places a group decision covers.",
        "Reviewer decisions",
    ),
    e(
        "Reason recorded",
        "Yes when the reviewer wrote a reason down.",
        "Reviewer decisions",
    ),
    e(
        "Revoked",
        "Yes when the decision was later undone. It stays in the record.",
        "Reviewer decisions",
    ),
    e(
        "Group",
        "A group of problems: an area, a rule or a fix.",
        "How problems were grouped",
    ),
    e(
        "Within",
        "What kind of group it is: Area, Rule or Fix.",
        "How problems were grouped",
    ),
    e(
        "Under",
        "The group this one sits under.",
        "How problems were grouped",
    ),
    e(
        "Cause",
        "Why the places of a fix belong together.",
        "How problems were grouped",
    ),
    e(
        "Summary",
        "A short account of the group.",
        "How problems were grouped",
    ),
    e(
        "Reviewed with others like it",
        "Places a reviewer ruled on as part of a group of places that share one cause.",
        "How problems were grouped",
    ),
    e(
        "Term",
        "The word being defined.",
        "Glossary",
    ),
    e(
        "Plain meaning",
        "What the word means in this package.",
        "Glossary",
    ),
    e(
        "Where you meet it",
        "The sheets and columns where the word shows up.",
        "Glossary",
    ),
    e(
        "Sheet",
        "A tab of this workbook.",
        "Start here",
    ),
    e(
        "What it is",
        "One line on what a sheet holds.",
        "Start here",
    ),
    e(
        "Rows",
        "How many rows a sheet holds.",
        "Start here",
    ),
    e(
        "Back to Start here",
        "A link at the top of every sheet that returns you to the Start here page.",
        "Every sheet",
    ),
    // The sheets.
    e(
        "Start here",
        "The first page: what this file is, a link to every sheet, the scorecard and the facts of the inspection.",
        "The first tab",
    ),
    e(
        "Glossary",
        "This sheet: what every word and column means.",
        "The second tab",
    ),
    e(
        "Fix plan",
        "One row for each fix, in the order to do them.",
        "The What to fix tabs",
    ),
    e(
        "All problems",
        "Every problem once, with the place it was seen and what to do there.",
        "The What to fix tabs",
    ),
    e(
        "Your conventions",
        "What your code already does consistently, and where it has no single habit.",
        "The What to fix tabs",
    ),
    e(
        "Departures",
        "Where your code departs from a convention it mostly follows.",
        "The What to fix tabs",
    ),
    e(
        "Dependencies",
        "Known vulnerabilities in the packages you depend on.",
        "The What to fix tabs",
    ),
    e(
        "Rules checked",
        "Every rule we ran and what it found.",
        "The What we checked tabs",
    ),
    e(
        "Differences across services",
        "The rules your services answer differently. It is an observation, so it carries no severity.",
        "The What we checked tabs, for projects of several repos",
    ),
    e(
        "Not checked",
        "What we could not or did not check: rules that could not finish, rules that did not run, files we did not read.",
        "The What we checked tabs",
    ),
    e(
        "All source rows",
        "The raw record: every row the engine produced, including the collapsed source rows behind a merged problem. This is the one sheet where a problem can be listed more than once.",
        "The For reviewers tabs",
    ),
    e(
        "Reviewer decisions",
        "Every decision a reviewer recorded, with the reason where it is shared with you.",
        "The For reviewers tabs",
    ),
    e(
        "How problems were grouped",
        "How problems were gathered into areas, rules and fixes, with the counts at each step.",
        "The For reviewers tabs",
    ),
    // The technical band.
    e(
        "Technical (ids for tooling)",
        "The grey columns at the right of every table, closed by default. Open them with the plus sign above the columns. They hold ids and codes for tools; you do not need them to read the sheet.",
        "The right edge of every table",
    ),
];

/// A glossary entry, spelled short.
const fn e(
    term: &'static str,
    meaning: &'static str,
    where_you_meet_it: &'static str,
) -> GlossaryEntry {
    GlossaryEntry {
        term,
        meaning,
        where_you_meet_it,
    }
}

/// Terms only a reviewer's export defines.
pub const INTERNAL_GLOSSARY_ENTRIES: &[GlossaryEntry] = &[
    e(
        "Escalated",
        "Yes when the group reviewer put the place in the architect's queue. Reviewers only.",
        "The technical columns of an internal export",
    ),
    e(
        "Escalation line",
        "The cut-off below which an escalated place is not read in person. Reviewers only.",
        "An internal export",
    ),
    e(
        "Review flags",
        "Notes the engine left on a row for a reviewer. Reviewers only.",
        "The technical columns of an internal export",
    ),
    e(
        "Held",
        "Why a row waits for a reviewer's ruling before a client may receive it. Reviewers only.",
        "The technical columns of an internal export",
    ),
];

// ── Words the package must never use for a thing named above ────────────────────────────

/// The old names. None of them may appear in a string a person reads in the workbook. The CSV
/// and JSON headers are machine-facing and keep their field names; a test strips file names such
/// as `findings.json` before it looks.
pub const BANNED_SYNONYMS: &[&str] = &[
    "site",
    "sites",
    "sub-group",
    "subgroup",
    "sub group",
    "remediation",
    "do now",
    "do next",
    "fix first",
    "backlog",
    "sequence",
    "finding",
    "findings",
    "headline",
    "candidate",
    "false positive",
    "false positives",
    "thrown out",
    "discarded",
    "category",
    "categories",
    "pattern",
    "patterns",
    "bucket",
    "disposition",
    "held for review",
    "informational",
    "provenance",
    "action-needed",
    "est. hours",
    "needs review",
];

/// Banned as written (case matters): the id header is "Rule id", never "Rule ID".
pub const BANNED_EXACT: &[&str] = &["Rule ID", "Site id", "Node id"];

/// The first banned synonym in `text`, if any. A word counts only as a whole word, so "website"
/// does not match "site"; machine file names are removed first.
pub fn banned_synonym_in(text: &str) -> Option<&'static str> {
    let mut cleaned = text.to_string();
    for file in ["all-findings", "findings.json", "false-positives"] {
        cleaned = cleaned.replace(file, " ");
    }
    for exact in BANNED_EXACT {
        if cleaned.contains(exact) {
            return Some(exact);
        }
    }
    let lower = cleaned.to_lowercase();
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
    for banned in BANNED_SYNONYMS {
        let mut from = 0;
        while let Some(at) = lower[from..].find(banned) {
            let start = from + at;
            let end = start + banned.len();
            let before_ok = lower[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_char(c));
            let after_ok = lower[end..].chars().next().is_none_or(|c| !is_word_char(c));
            if before_ok && after_ok {
                return Some(banned);
            }
            from = end;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glossary_terms() -> Vec<String> {
        GLOSSARY_ENTRIES
            .iter()
            .chain(INTERNAL_GLOSSARY_ENTRIES)
            .map(|e| e.term.to_lowercase())
            .collect()
    }

    #[test]
    fn every_sheet_name_is_a_glossary_term() {
        let terms = glossary_terms();
        for sheet in &SHEETS {
            assert!(
                terms.contains(&sheet.name.to_lowercase()),
                "the sheet {:?} has no Glossary entry",
                sheet.name
            );
        }
    }

    #[test]
    fn every_sheet_name_fits_excel_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for sheet in &SHEETS {
            assert!(
                sheet.name.chars().count() <= SHEET_NAME_MAX_CHARS,
                "{:?} is longer than Excel allows",
                sheet.name
            );
            assert!(seen.insert(sheet.name), "{:?} is named twice", sheet.name);
        }
    }

    #[test]
    fn the_glossary_names_each_term_once() {
        let terms = glossary_terms();
        let unique: std::collections::HashSet<_> = terms.iter().collect();
        assert_eq!(
            terms.len(),
            unique.len(),
            "a Glossary term is defined twice"
        );
    }

    #[test]
    fn the_core_vocabulary_of_the_proposal_is_defined() {
        let terms = glossary_terms();
        for term in [
            "fix",
            "place",
            "problem",
            "area",
            "rule",
            "rule id",
            "fix order",
            "severity",
            "effort",
            "estimated hours",
            "confidence",
            "needs a decision",
            "ruled out",
            "convention",
            "departure",
            "deterministic",
            "model-reviewed",
            "related standard",
            "reviewed in person",
            "engine verified",
            "example from your code",
            "standard fix",
            "kind",
            "carried from an earlier inspection",
            "dependency advisory",
            "status",
        ] {
            assert!(
                terms.contains(&term.to_string()),
                "the Glossary does not define {term:?}"
            );
        }
    }

    #[test]
    fn fix_order_has_one_set_of_words() {
        assert_eq!(
            [FIX_ORDER_FIRST, FIX_ORDER_NEXT, FIX_ORDER_LATER],
            ["First", "Next", "Later"]
        );
    }

    #[test]
    fn the_detail_map_names_only_real_sheets() {
        for (question, sheet) in DETAIL_MAP {
            assert!(
                sheet_info(sheet).is_some(),
                "{question:?} points at {sheet:?}, which is not a sheet"
            );
        }
    }

    #[test]
    fn no_vocabulary_string_uses_a_banned_synonym() {
        let mut strings: Vec<String> = Vec::new();
        for sheet in &SHEETS {
            strings.push(sheet.name.to_string());
            strings.push(sheet.purpose.to_string());
        }
        for e in GLOSSARY_ENTRIES.iter().chain(INTERNAL_GLOSSARY_ENTRIES) {
            strings.push(e.term.to_string());
            strings.push(e.meaning.to_string());
            strings.push(e.where_you_meet_it.to_string());
        }
        for (question, _) in DETAIL_MAP {
            strings.push(question.to_string());
        }
        for s in &strings {
            assert_eq!(banned_synonym_in(s), None, "{s:?} uses an old name");
        }
    }

    #[test]
    fn the_banned_check_matches_whole_words_only() {
        assert_eq!(banned_synonym_in("Do now"), Some("do now"));
        assert_eq!(
            banned_synonym_in("the Remediation plan"),
            Some("remediation")
        );
        assert_eq!(banned_synonym_in("a Sub-group"), Some("sub-group"));
        assert_eq!(banned_synonym_in("Sites sheet"), Some("sites"));
        assert_eq!(banned_synonym_in("Rule ID"), Some("Rule ID"));
        assert_eq!(banned_synonym_in("a website"), None);
        assert_eq!(banned_synonym_in("see findings.json"), None);
    }

    #[test]
    fn no_dash_character_is_in_the_vocabulary() {
        let mut strings: Vec<&str> = Vec::new();
        for sheet in &SHEETS {
            strings.push(sheet.purpose);
        }
        for e in GLOSSARY_ENTRIES.iter().chain(INTERNAL_GLOSSARY_ENTRIES) {
            strings.push(e.meaning);
            strings.push(e.where_you_meet_it);
        }
        for s in strings {
            assert!(
                !s.contains(['\u{2014}', '\u{2013}', '\u{00D7}']),
                "{s:?} carries an em dash, an en dash or a multiplication sign"
            );
        }
    }
}
