//! THE GATED CONVENTION COMMIT — the pure half (design §3, §12 Q3/Q4, §13's rulings).
//!
//! The audit derives the client's conventions from the client's own codebase. ADOPTING one
//! writes it into that repository's `AGENTS.md`, where it governs how future AI agents work on
//! the codebase. This module owns every decision in that act that is not a pixel: what is
//! adoptable, what is adopted, what was actually LOOKED AT, what the commit will write, and
//! whether the gate is open.
//!
//! ## The three rulings this implements, verbatim
//!
//! > "Yes it should be opt out by default, make sure it is thoroughly gated and not an
//! > accidental click."
//!
//! Opt-out: [`AdoptionModel`] stores SKIPS, so a convention nobody touched is adopted. The gate
//! is [`commit_gate`], and it is a typed destination rather than a checkbox — see its doc for
//! why that distinction is the whole point.
//!
//! > "I could decide that 'this is the correct convention even though there are inconsistencies
//! > that exist'. The remediation of those inconsistencies is a separate process. But if I say
//! > 'this is the rule and convention' then I mean it, and that can be adopted."
//!
//! A convention with live deviations IS adoptable. [`adoptable_conventions`] draws from BOTH
//! `ConventionObserved` rows and the convention BEHIND a rule's `Deviation` rows, and
//! [`AdoptableConvention::deviating_sites`] is carried as CONTEXT — nothing in this module
//! reads it to decide anything. Adopting states what the standard is going forward; whether
//! the code complies today is separate work with a separate owner, and the repo with the most
//! drift is the one that most needs the convention written down.
//!
//! > Q3 DECIDED — a `NoConvention` option pick "feeds the derived decision set, so it is a
//! > governance act and must say so in the UI" and "inherits Q4's gating".
//!
//! So a recommendation and an adoption produce the SAME thing — one [`AdoptedRule`] — travel
//! the same [`CommitPlan`], and pass the same [`commit_gate`]. The only difference is the
//! default: a convention defaults to adopted, a recommendation defaults to absent.
//!
//! ## How an adoption reaches `AGENTS.md`
//!
//! Through the path that already exists, with no new server surface:
//!
//! 1. `GET /api/projects/:id/ruleset` — the project's current [`ProjectRulesetView`].
//! 2. [`merge_adoptions`] folds the adopted rules in. This is a READ-MODIFY-WRITE because the
//!    server's `upsert_base_rules` REPLACES `selections` wholesale; sending only the adopted
//!    rules would delete every other selection the project has.
//! 3. `POST /api/projects/:id/ruleset` — upserts the base rules, preserving custom rules.
//! 4. `POST /api/projects/:id/emit-local` with the cascade all-false — `camerata_server::arm`
//!    renders `AGENTS.md` / `CONVENTIONS.md` / the gate config into the repo's local checkout.
//!
//! Step 4 is the existing emit path, unchanged. An adopted convention is a `RuleSelection` with
//! a `chosen_option`, which is exactly what `arm::arm_files_for_repo` already knows how to
//! render, so nothing here invents a format or a destination.

use crate::triage::FindingView;
use camerata_api_types::audit::FindingKind;
use std::collections::{BTreeMap, BTreeSet};

/// Separator inside an [`adoption_key`]. A NUL can appear in neither a repo name nor a rule id,
/// so the key is unambiguous — the same device [`crate::triage::finding_key`] uses.
const KEY_SEP: char = '\u{0}';

/// Stable identity of one adoptable decision: the `(repo, rule_id)` pair.
///
/// Scoped per repo for the same reason [`crate::triage::rules_on_multiple_surfaces`] is: in a
/// multi-repo scan one repo can honestly follow a convention while another does not, and those
/// are two separate adoptions into two separate `AGENTS.md` files.
pub fn adoption_key(repo: &str, rule_id: &str) -> String {
    format!("{repo}{KEY_SEP}{rule_id}")
}

/// ONE CONVENTION THE CURATOR MAY ADOPT into a client repository.
///
/// Flattened from the scan's own rows. Two kinds produce one: a `ConventionObserved` row (the
/// codebase follows it with zero violations) and the convention BEHIND a rule's `Deviation`
/// rows (the codebase mostly follows it). Both are conventions the audit derived; the second
/// one merely also has outstanding work attached, which is a different owner's problem.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AdoptableConvention {
    /// `owner/repo` — the DESTINATION. Its `AGENTS.md` is what adoption writes.
    pub repo: String,
    /// The corpus rule this convention is an option of.
    pub rule_id: String,
    /// The authored option id the codebase follows.
    ///
    /// `None` only for a deviation whose producer established a majority convention OUTSIDE
    /// the authored option set (`DeviationKind::convention_option_id` is itself optional). Such
    /// a convention is NOT adoptable — see [`Self::is_adoptable`] — because there is no
    /// structured identity to write, and writing the rule's DEFAULT option instead would
    /// install a directive the audit never observed.
    pub option_id: Option<String>,
    /// The authored option's human label, corpus text and never model prose.
    pub label: String,
    /// How many sites deviate from this convention right now. **Context, never a gate.**
    /// Zero for a `ConventionObserved` row.
    pub deviating_sites: usize,
}

impl AdoptableConvention {
    /// This convention's [`adoption_key`].
    pub fn key(&self) -> String {
        adoption_key(&self.repo, &self.rule_id)
    }

    /// Whether this convention can be written at all.
    ///
    /// The ONLY disqualifier is a missing authored option id, and that is a data limit rather
    /// than a judgment: there is no structured directive to emit. Live deviations are
    /// explicitly NOT a disqualifier — that is the owner's reversal of the original design, and
    /// `req_pass2_14_a_convention_with_live_deviations_is_still_adoptable` pins it.
    pub fn is_adoptable(&self) -> bool {
        self.option_id.is_some()
    }

    /// How the panel states this convention's standing: observed clean, or observed with N
    /// exceptions (design §13's exact phrase).
    ///
    /// Deliberately phrased so the exception count reads as CONTEXT attached to an established
    /// convention, not as an objection to adopting it.
    pub fn status_line(&self) -> String {
        match self.deviating_sites {
            0 => "observed, no exceptions".to_string(),
            1 => "observed with 1 exception".to_string(),
            n => format!("observed with {n} exceptions"),
        }
    }

    /// Why this convention cannot be adopted, or `None` when it can.
    pub fn blocked_reason(&self) -> Option<&'static str> {
        if self.is_adoptable() {
            None
        } else {
            Some(
                "No authored option matches the observed convention, so there is nothing \
                 structured to write. Adopting would install this rule's default directive \
                 instead, which the audit never observed.",
            )
        }
    }
}

/// Every convention this scan derived, in a stable `(repo, rule_id)` order.
///
/// Draws from both sources and keys them together, so a rule that somehow emitted BOTH a
/// convention row and deviation rows (the pipeline bug
/// [`crate::triage::rules_on_multiple_surfaces`] already shouts about) yields ONE entry rather
/// than two competing ones — the `ConventionObserved` row wins the label, and the deviation
/// count still attaches, so the conflict is visible rather than doubled.
pub fn adoptable_conventions(findings: &[FindingView]) -> Vec<AdoptableConvention> {
    // (repo, rule_id) -> entry. BTreeMap for the stable order.
    let mut out: BTreeMap<(String, String), AdoptableConvention> = BTreeMap::new();
    // First pass: the deviations, which carry the counts.
    for f in findings {
        let Some(FindingKind::Deviation(d)) = f.kind.as_ref() else {
            continue;
        };
        let e = out
            .entry((f.repo.clone(), f.rule_id.clone()))
            .or_insert_with(|| AdoptableConvention {
                repo: f.repo.clone(),
                rule_id: f.rule_id.clone(),
                option_id: d.convention_option_id.clone(),
                label: d.convention_label.clone(),
                deviating_sites: d.deviating_sites,
            });
        // Every deviating row of a rule repeats the same group-level counts verbatim
        // (`DeviationKind`'s own doc), so later rows agree with the first and this is a no-op.
        e.deviating_sites = d.deviating_sites;
    }
    // Second pass: the clean conventions. A ConventionObserved row carries a NON-optional
    // option id, so it is the stronger identity and overwrites a deviation-derived label.
    for f in findings {
        if let Some(FindingKind::ConventionObserved(c)) = f.kind.as_ref() {
            let e = out
                .entry((f.repo.clone(), f.rule_id.clone()))
                .or_insert_with(|| AdoptableConvention {
                    repo: f.repo.clone(),
                    rule_id: f.rule_id.clone(),
                    option_id: Some(c.convention_option_id.clone()),
                    label: c.convention_label.clone(),
                    deviating_sites: 0,
                });
            e.option_id = Some(c.convention_option_id.clone());
            e.label = c.convention_label.clone();
        }
    }
    out.into_values().collect()
}

/// ONE RULE WHERE THE CODEBASE IS SPLIT, and the options it was seen using.
///
/// The `NoConvention` panel's unit of decision. Picking one of `options` is a CLIENT-FACING
/// governance act (Q3), not curator scratch state: it reaches the same `AGENTS.md` by the same
/// path as an adoption.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SplitRule {
    /// `owner/repo` — the destination.
    pub repo: String,
    /// The corpus rule the codebase is split on.
    pub rule_id: String,
    /// The authored options the codebase was observed using, with their site counts where the
    /// producer counted them. `(option_id, label, sites)`.
    pub options: Vec<(String, String, Option<usize>)>,
}

impl SplitRule {
    /// This rule's [`adoption_key`].
    pub fn key(&self) -> String {
        adoption_key(&self.repo, &self.rule_id)
    }

    /// The authored label for `option_id`, when it is one of the observed options.
    pub fn label_of(&self, option_id: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(id, _, _)| id == option_id)
            .map(|(_, label, _)| label.as_str())
    }

    /// How an option reads in the picker: its label, plus its site count when the producer
    /// counted. An uncounted option is its bare label — never a fabricated zero.
    pub fn option_line(&self, option_id: &str) -> Option<String> {
        self.options.iter().find(|(id, _, _)| id == option_id).map(
            |(_, label, sites)| match sites {
                Some(n) => format!("{label} ({n} sites)"),
                None => label.clone(),
            },
        )
    }
}

/// Every split rule in the scan, in a stable `(repo, rule_id)` order.
pub fn split_rules(findings: &[FindingView]) -> Vec<SplitRule> {
    let mut out: BTreeMap<(String, String), SplitRule> = BTreeMap::new();
    for f in findings {
        if let Some(FindingKind::NoConvention(n)) = f.kind.as_ref() {
            out.entry((f.repo.clone(), f.rule_id.clone()))
                .or_insert_with(|| SplitRule {
                    repo: f.repo.clone(),
                    rule_id: f.rule_id.clone(),
                    options: n
                        .observed_options
                        .iter()
                        .map(|o| (o.option_id.clone(), o.label.clone(), o.sites))
                        .collect(),
                });
        }
    }
    out.into_values().collect()
}

/// THE CURATOR'S ADOPTION STATE, and the honest record of what he actually looked at.
///
/// ## Why this stores SKIPS rather than adoptions
///
/// Because adoption is opt-out. Storing adoptions would make "the curator has not reacted yet"
/// indistinguishable from "the curator declined", and the default would then depend on
/// whichever the renderer assumed. Storing skips makes the default a property of the data
/// structure: an untouched key is adopted, and there is no second place for that rule to live.
///
/// ## Why `opened` is a separate set from `skipped`
///
/// The commit dialog's load-bearing number is "how many were adopted by default and never
/// opened". It has to be a record of a gesture that actually happened, because the whole point
/// of showing it is that the curator cannot otherwise know. So [`Self::mark_opened`] is called
/// by exactly one thing — an explicit open of that convention's detail — and nothing derives
/// openness from adoption, from rendering, or from scroll position.
///
/// A row that was toggled Skip-then-Adopt-back without ever being opened still counts as
/// UNOPENED. That is deliberate: the curator selected it but did not read it, and the gate errs
/// toward telling him more rather than less.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AdoptionModel {
    /// Conventions explicitly declined. Absence means adopted (the opt-out default).
    skipped: BTreeSet<String>,
    /// Decisions whose detail the curator explicitly opened. Written by one gesture only.
    opened: BTreeSet<String>,
    /// `NoConvention` picks: key -> chosen option id. Absence means "leave as reported".
    recommended: BTreeMap<String, String>,
}

impl AdoptionModel {
    /// A fresh model: nothing skipped, nothing opened, nothing recommended.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this convention will be written. `true` for anything untouched — the opt-out
    /// default.
    pub fn is_adopted(&self, key: &str) -> bool {
        !self.skipped.contains(key)
    }

    /// Set this convention's adopt/skip state explicitly.
    pub fn set_adopted(&mut self, key: &str, adopt: bool) {
        if adopt {
            self.skipped.remove(key);
        } else {
            self.skipped.insert(key.to_string());
        }
    }

    /// Flip adopt/skip.
    pub fn toggle_adopted(&mut self, key: &str) {
        let now = self.is_adopted(key);
        self.set_adopted(key, !now);
    }

    /// Record that the curator OPENED this decision's detail. The only writer of the openness
    /// record; see the type doc.
    pub fn mark_opened(&mut self, key: &str) {
        self.opened.insert(key.to_string());
    }

    /// Whether the curator opened this decision's detail.
    pub fn was_opened(&self, key: &str) -> bool {
        self.opened.contains(key)
    }

    /// How many decisions have been opened in total. Diagnostic; the dialog reads the
    /// per-destination figure off a [`CommitPlan`] instead.
    pub fn opened_count(&self) -> usize {
        self.opened.len()
    }

    /// The option recommended for a split rule, or `None` for "leave as reported".
    pub fn recommendation(&self, key: &str) -> Option<&str> {
        self.recommended.get(key).map(String::as_str)
    }

    /// Recommend an option for a split rule; `None` returns it to "leave as reported".
    pub fn set_recommendation(&mut self, key: &str, option_id: Option<String>) {
        match option_id {
            Some(id) if !id.trim().is_empty() => {
                self.recommended.insert(key.to_string(), id);
            }
            _ => {
                self.recommended.remove(key);
            }
        }
    }
}

/// ONE RULE A COMMIT WILL WRITE into the destination's `AGENTS.md`.
///
/// The common currency of both panels: an adopted convention and a recommended option for a
/// split rule produce the identical thing, which is precisely why Q3 says the second one is a
/// governance act too.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AdoptedRule {
    /// The corpus rule id.
    pub rule_id: String,
    /// The authored option to install as this rule's position. Always `Some` in a plan —
    /// [`CommitPlan::build`] drops anything without one, because there would be nothing
    /// structured to write.
    pub option_id: String,
    /// The authored option's human label, for the dialog's own listing.
    pub label: String,
    /// Whether the curator opened this rule's detail before committing it.
    pub opened: bool,
}

/// WHAT A COMMIT WOULD DO, stated completely, for one destination repository.
///
/// Built before the dialog opens and handed to it as a prop, so the dialog has no derivation of
/// its own and SSR can render every state. Every number the dialog shows is a field or a method
/// here, which is what makes them testable without a browser.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommitPlan {
    /// `owner/repo` — the destination whose `AGENTS.md` this writes, and the string the curator
    /// has to type. Named explicitly per the ruling.
    pub destination_repo: String,
    /// What is being written, plural, for the dialog's sentences: `"conventions"` or
    /// `"recommendations"`.
    pub noun: String,
    /// The rules that will be written, in panel order.
    pub adopted: Vec<AdoptedRule>,
    /// How many of this destination's decisions the curator declined. Not written; shown so
    /// the dialog accounts for the whole set rather than only the part it is acting on.
    pub skipped: usize,
}

impl CommitPlan {
    /// How many rules this writes.
    pub fn adopted_count(&self) -> usize {
        self.adopted.len()
    }

    /// **THE NUMBER THAT MATTERS.** How many of the rules being written were never opened.
    ///
    /// Counted off the per-rule [`AdoptedRule::opened`] flags that [`Self::build`] read from
    /// the [`AdoptionModel`]'s openness record, so it tracks what the curator actually did. It
    /// is not a constant, not a ratio and not an estimate: open one more convention and this
    /// falls by exactly one.
    pub fn unopened_count(&self) -> usize {
        self.adopted.iter().filter(|r| !r.opened).count()
    }

    /// The rule ids being written without having been opened — what the dialog's third exit
    /// ("Review the unopened set") filters the panel down to.
    pub fn unopened_rule_ids(&self) -> Vec<String> {
        self.adopted
            .iter()
            .filter(|r| !r.opened)
            .map(|r| r.rule_id.clone())
            .collect()
    }

    /// Nothing to write. The dialog must not open on this, and the gate refuses it.
    pub fn is_empty(&self) -> bool {
        self.adopted.is_empty()
    }

    /// The dialog's opening sentence — the approved copy, with the real numbers in it.
    ///
    /// > "You are writing 36 conventions into a client repository. This changes how future AI
    /// > agents working on that codebase will behave."
    pub fn headline(&self) -> String {
        let n = self.adopted_count();
        let noun = if n == 1 {
            singularize(&self.noun)
        } else {
            self.noun.clone()
        };
        format!(
            "You are writing {n} {noun} into a client repository. This changes how future AI \
             agents working on that codebase will behave."
        )
    }

    /// The dialog's warning — the approved copy — or `None` when every rule was opened.
    ///
    /// > "25 conventions were adopted by default and not opened. Adoption is opt-out, so these
    /// > were selected for you."
    ///
    /// Returning `None` rather than a zero sentence matters: a standing "0 were not opened"
    /// line trains the eye to skip the row, and this row is the one the gate exists to make
    /// unskippable.
    pub fn unopened_warning(&self) -> Option<String> {
        let n = self.unopened_count();
        if n == 0 {
            return None;
        }
        let noun = if n == 1 {
            singularize(&self.noun)
        } else {
            self.noun.clone()
        };
        let verb = if n == 1 { "was" } else { "were" };
        Some(format!(
            "{n} {noun} {verb} adopted by default and not opened. Adoption is opt-out, so \
             these were selected for you."
        ))
    }

    /// The count line: what is written, what was declined, where it lands.
    pub fn counts_line(&self) -> String {
        format!(
            "{} to write \u{b7} {} skipped \u{b7} destination {}",
            self.adopted_count(),
            self.skipped,
            self.destination_repo
        )
    }

    /// Build the plan for one destination out of the scan's conventions and the curator's
    /// state.
    ///
    /// Drops a convention that is not [`AdoptableConvention::is_adoptable`] even when it is
    /// "adopted": there is no option id to write, so including it would put a rule in the
    /// dialog's count that the commit could not honor.
    pub fn conventions(
        all: &[AdoptableConvention],
        model: &AdoptionModel,
        destination_repo: &str,
    ) -> Self {
        let mine: Vec<&AdoptableConvention> =
            all.iter().filter(|c| c.repo == destination_repo).collect();
        let mut adopted = Vec::new();
        let mut skipped = 0usize;
        for c in &mine {
            let key = c.key();
            if !model.is_adopted(&key) {
                skipped += 1;
                continue;
            }
            let Some(option_id) = c.option_id.clone() else {
                continue;
            };
            adopted.push(AdoptedRule {
                rule_id: c.rule_id.clone(),
                option_id,
                label: c.label.clone(),
                opened: model.was_opened(&key),
            });
        }
        Self {
            destination_repo: destination_repo.to_string(),
            noun: "conventions".to_string(),
            adopted,
            skipped,
        }
    }

    /// Build the plan for one destination out of the scan's split rules and the curator's
    /// picks.
    ///
    /// The default here is the OPPOSITE of a convention's: a split rule with no pick is left as
    /// reported and is counted under `skipped`. It still gets the same dialog and the same
    /// typed gate, because what it writes is the same thing (Q3).
    pub fn recommendations(
        all: &[SplitRule],
        model: &AdoptionModel,
        destination_repo: &str,
    ) -> Self {
        let mine: Vec<&SplitRule> = all.iter().filter(|r| r.repo == destination_repo).collect();
        let mut adopted = Vec::new();
        let mut skipped = 0usize;
        for r in &mine {
            let key = r.key();
            match model.recommendation(&key) {
                Some(option_id) => adopted.push(AdoptedRule {
                    rule_id: r.rule_id.clone(),
                    option_id: option_id.to_string(),
                    label: r.label_of(option_id).unwrap_or(option_id).to_string(),
                    opened: model.was_opened(&key),
                }),
                None => skipped += 1,
            }
        }
        Self {
            destination_repo: destination_repo.to_string(),
            noun: "recommendations".to_string(),
            adopted,
            skipped,
        }
    }
}

/// Crude plural -> singular for the two nouns this module uses. Private, and deliberately not
/// a general pluralizer: it handles `conventions` and `recommendations` and falls through
/// unchanged for anything else.
fn singularize(noun: &str) -> String {
    noun.strip_suffix('s').unwrap_or(noun).to_string()
}

/// THE STATE OF THE COMMIT GATE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommitGate {
    /// The plan writes nothing. There is no commit to arm.
    NothingToCommit,
    /// Nothing has been typed yet.
    AwaitingDestination,
    /// Something was typed and it is not the destination. **The commit stays shut.**
    Mismatch,
    /// The destination was typed. The commit may proceed.
    Armed,
}

impl CommitGate {
    /// Whether the commit may proceed. The single predicate every caller must consult.
    pub fn is_armed(self) -> bool {
        matches!(self, CommitGate::Armed)
    }

    /// What the dialog tells the curator about why the button is shut.
    pub fn hint(self, destination_repo: &str) -> Option<String> {
        match self {
            CommitGate::Armed => None,
            CommitGate::NothingToCommit => Some("Nothing is selected to write.".to_string()),
            CommitGate::AwaitingDestination => Some(format!(
                "Type {destination_repo} to confirm the destination."
            )),
            CommitGate::Mismatch => Some(format!(
                "That is not {destination_repo}. The commit stays closed until the destination \
                 repository name matches."
            )),
        }
    }
}

/// THE GATE (owner's Q4: "thoroughly gated and not an accidental click").
///
/// ## Why a typed destination and not a checkbox
///
/// A checkbox is muscle memory at 1am — the hand learns where it is and clicks it without the
/// eye involved. Typing `owner/repo` cannot be done without reading `owner/repo`, which is the
/// one fact a curator about to write into a client's repository must have in mind. The gate is
/// not there to be difficult; it is there to force the read.
///
/// ## What counts as a match
///
/// Surrounding whitespace is trimmed (a pasted name carries it) and the comparison is
/// ASCII-case-insensitive (GitHub treats repository names that way, and a case mismatch would
/// be a frustration with no safety value — the whole name still has to be read to type it).
/// Nothing else is forgiven: not a prefix, not a suffix, not the bare repo without its owner,
/// not a different repository in the same scan. `req_pass2_14_the_commit_gate_cannot_be_
/// bypassed` walks those cases.
///
/// An empty plan can never arm, whatever is typed — there is nothing to confirm.
pub fn commit_gate(plan: &CommitPlan, typed: &str) -> CommitGate {
    if plan.is_empty() {
        return CommitGate::NothingToCommit;
    }
    let typed = typed.trim();
    if typed.is_empty() {
        return CommitGate::AwaitingDestination;
    }
    if typed.eq_ignore_ascii_case(plan.destination_repo.trim()) {
        CommitGate::Armed
    } else {
        CommitGate::Mismatch
    }
}

// ── The ruleset wire, mirrored ───────────────────────────────────────────────

/// One base rule selection, mirroring `camerata_app_core::project::RuleSelection` FIELD FOR
/// FIELD.
///
/// Re-declared rather than imported for the same reason [`crate::triage::MergedSiteView`] is:
/// `camerata-ui-core` must not depend on the server. The field-for-field part is load-bearing
/// and not cosmetic — the commit is a READ-MODIFY-WRITE of the whole `selections` array, so a
/// field this struct forgets is a field the commit SILENTLY ERASES from every selection it
/// round-trips. `applied_hash`, `applied_directive` and `applied_version` are the rule-drift
/// baseline; dropping them would make every rule in the project report as never-baselined after
/// one convention commit. `req_pass2_14_the_ruleset_round_trip_preserves_every_selection_field`
/// pins it.
#[derive(Clone, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RuleSelectionView {
    /// The rule id.
    pub rule_id: String,
    /// The chosen alternative option id, when the rule has alternatives.
    #[serde(default)]
    pub chosen_option: Option<String>,
    /// The repos this rule installs into.
    #[serde(default)]
    pub repos: Vec<String>,
    /// The corpus content-hash baselined for this selection. Passed through untouched.
    #[serde(default)]
    pub applied_hash: Option<String>,
    /// The resolved directive as of the last baseline. Passed through untouched.
    #[serde(default)]
    pub applied_directive: Option<String>,
    /// The corpus rule's `version` as of the last baseline. Passed through untouched.
    #[serde(default)]
    pub applied_version: Option<u32>,
}

/// The project's base ruleset as the commit reads and rewrites it.
///
/// `custom` is deliberately ABSENT. The server's `import_project_ruleset` calls
/// `upsert_base_rules` (which replaces these three lists) and then `merge_custom` (which merges
/// by name and never drops), so omitting the key entirely leaves every custom rule exactly
/// where it was. Modeling `custom` here would mean round-tripping architect-authored prose
/// through a struct that has no reason to touch it — more surface, no benefit, and one more
/// place a field could be dropped.
#[derive(Clone, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ProjectRulesetView {
    /// Repo-local rule selections.
    #[serde(default)]
    pub selections: Vec<RuleSelectionView>,
    /// Cross-repo (API contract) selections. Passed through; searched for an existing rule.
    #[serde(default)]
    pub cross_repo: Vec<RuleSelectionView>,
    /// Process (commit/PR) selections. Passed through; searched for an existing rule.
    #[serde(default)]
    pub process: Vec<RuleSelectionView>,
}

/// Fold a commit's adopted rules into the project's existing ruleset.
///
/// Pure, so the thing that decides what a client's `AGENTS.md` will say is unit-tested rather
/// than inferred from a network trace.
///
/// Rules:
///
/// * An existing selection for the rule — in ANY of the three lists, matched
///   case-insensitively the way the server's own `set_rule_chosen_option` matches — is UPDATED
///   in place: its `chosen_option` becomes the adopted option, and the destination repo is
///   unioned into its `repos`. It is not moved between lists; a cross-repo rule stays
///   cross-repo.
/// * A rule with no existing selection is APPENDED to `selections` with the destination as its
///   only repo, which is exactly the shape `save_armed_to_project` writes.
/// * Nothing is ever removed, and no field of an existing selection other than `chosen_option`
///   and `repos` is touched.
pub fn merge_adoptions(
    current: &ProjectRulesetView,
    destination_repo: &str,
    adopted: &[AdoptedRule],
) -> ProjectRulesetView {
    let mut out = current.clone();
    for rule in adopted {
        let want = rule.rule_id.trim().to_ascii_uppercase();
        let mut found = false;
        for list in [&mut out.selections, &mut out.cross_repo, &mut out.process] {
            if let Some(sel) = list
                .iter_mut()
                .find(|s| s.rule_id.trim().to_ascii_uppercase() == want)
            {
                sel.chosen_option = Some(rule.option_id.clone());
                if !sel.repos.iter().any(|r| r == destination_repo) {
                    sel.repos.push(destination_repo.to_string());
                }
                found = true;
                break;
            }
        }
        if !found {
            out.selections.push(RuleSelectionView {
                rule_id: rule.rule_id.clone(),
                chosen_option: Some(rule.option_id.clone()),
                repos: vec![destination_repo.to_string()],
                applied_hash: None,
                applied_directive: None,
                applied_version: None,
            });
        }
    }
    out
}

/// Every destination repository the scan's conventions could be written into, sorted.
pub fn convention_destinations(all: &[AdoptableConvention]) -> Vec<String> {
    let mut repos: Vec<String> = all.iter().map(|c| c.repo.clone()).collect();
    repos.sort();
    repos.dedup();
    repos
}

/// Every destination repository the scan's split rules could be written into, sorted.
pub fn split_destinations(all: &[SplitRule]) -> Vec<String> {
    let mut repos: Vec<String> = all.iter().map(|r| r.repo.clone()).collect();
    repos.sort();
    repos.dedup();
    repos
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finding carrying an arbitrary flattened `kind` payload, the same construction the
    /// triage module's own kind tests use.
    fn kinded(repo: &str, rule_id: &str, kind: serde_json::Value) -> FindingView {
        let mut row = serde_json::json!({
            "repo": repo,
            "path": "src/lib.rs",
            "line": 1,
            "rule_id": rule_id,
            "severity": "info",
            "snippet": "",
            "detail": "",
        });
        let obj = row.as_object_mut().expect("object");
        for (k, v) in kind.as_object().expect("kind object") {
            obj.insert(k.clone(), v.clone());
        }
        serde_json::from_value(row).expect("FindingView")
    }

    fn convention(repo: &str, rule_id: &str, option: &str, label: &str) -> FindingView {
        kinded(
            repo,
            rule_id,
            serde_json::json!({
                "kind": "convention_observed",
                "convention_option_id": option,
                "convention_label": label,
            }),
        )
    }

    fn deviation(
        repo: &str,
        rule_id: &str,
        deviating: usize,
        assessed: Option<usize>,
        option: Option<&str>,
        label: &str,
    ) -> FindingView {
        kinded(
            repo,
            rule_id,
            serde_json::json!({
                "kind": "deviation",
                "deviating_sites": deviating,
                "assessed_sites": assessed,
                "convention_option_id": option,
                "convention_label": label,
            }),
        )
    }

    fn split(repo: &str, rule_id: &str, options: serde_json::Value) -> FindingView {
        kinded(
            repo,
            rule_id,
            serde_json::json!({ "kind": "no_convention", "observed_options": options }),
        )
    }

    // ── The adoptable set ────────────────────────────────────────────────────

    /// req_pass2_14 (THE OWNER'S REVERSAL, design §13): a convention with LIVE DEVIATIONS is
    /// adoptable. The design originally blocked adoption until the deviations were
    /// dispositioned; the owner reversed it:
    ///
    /// > "I could decide that 'this is the correct convention even though there are
    /// > inconsistencies that exist'. … if I say 'this is the rule and convention' then I mean
    /// > it, and that can be adopted."
    ///
    /// The count is carried as context and the status line states it, but NOTHING reads it to
    /// decide: the convention is adoptable, it reaches the plan, and the gate arms over it.
    #[test]
    fn req_pass2_14_a_convention_with_live_deviations_is_still_adoptable() {
        let rows = vec![
            deviation(
                "me/api",
                "API-VERSIONING-1",
                2,
                Some(47),
                Some("versioned"),
                "URI-versioned endpoints",
            ),
            deviation(
                "me/api",
                "API-VERSIONING-1",
                2,
                Some(47),
                Some("versioned"),
                "URI-versioned endpoints",
            ),
        ];
        let all = adoptable_conventions(&rows);
        assert_eq!(
            all.len(),
            1,
            "one convention per (repo, rule), not per row: {all:?}"
        );
        let c = &all[0];
        assert_eq!(c.deviating_sites, 2, "the count is carried");
        assert!(c.is_adoptable(), "live deviations must NOT block adoption");
        assert_eq!(c.blocked_reason(), None);
        assert_eq!(
            c.status_line(),
            "observed with 2 exceptions",
            "§13's exact phrase"
        );

        // It reaches the plan under the opt-out default, and the gate arms over it.
        let model = AdoptionModel::new();
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        assert_eq!(
            plan.adopted_count(),
            1,
            "a deviating convention is in the commit: {plan:?}"
        );
        assert_eq!(plan.adopted[0].option_id, "versioned");
        assert!(
            commit_gate(&plan, "me/api").is_armed(),
            "and it can actually be committed"
        );
    }

    /// req_pass2_14: a deviation-backed convention is adoptable at ANY ratio — the design's
    /// original "resolve them first" stance would have made the worst-drift repo the hardest
    /// to govern, which is the exact failure §13 names.
    #[test]
    fn req_pass2_14_adoptability_does_not_vary_with_the_deviation_ratio() {
        for (deviating, assessed) in [(1, Some(100)), (19, Some(47)), (46, Some(47)), (3, None)] {
            let rows = vec![deviation(
                "me/api",
                "R-1",
                deviating,
                assessed,
                Some("opt"),
                "Some convention",
            )];
            let all = adoptable_conventions(&rows);
            assert!(
                all[0].is_adoptable(),
                "{deviating} of {assessed:?} must still be adoptable"
            );
            let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "me/api");
            assert_eq!(plan.adopted_count(), 1, "{deviating}/{assessed:?}");
        }
    }

    /// req_pass2_14 (the ONE honest disqualifier): a deviation whose producer established a
    /// majority convention OUTSIDE the authored option set has no option id, so there is
    /// nothing structured to write. It is listed and explained, never silently committed as
    /// the rule's default option.
    #[test]
    fn req_pass2_14_a_convention_with_no_authored_option_is_not_written() {
        let rows = vec![deviation(
            "me/api",
            "R-1",
            2,
            Some(9),
            None,
            "something the model saw",
        )];
        let all = adoptable_conventions(&rows);
        assert_eq!(all.len(), 1, "still LISTED — the curator should see it");
        assert!(!all[0].is_adoptable());
        assert!(all[0].blocked_reason().is_some(), "and told why");
        let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "me/api");
        assert!(
            plan.is_empty(),
            "but it must not reach the commit: {plan:?}"
        );
        assert_eq!(commit_gate(&plan, "me/api"), CommitGate::NothingToCommit);
    }

    /// req_pass2_14: a clean `ConventionObserved` row and a deviation-backed convention sit in
    /// ONE list, keyed per (repo, rule), ordered stably.
    #[test]
    fn req_pass2_14_adoptable_set_draws_from_both_kinds() {
        let rows = vec![
            convention("me/api", "B-RULE", "b", "B convention"),
            deviation("me/api", "A-RULE", 3, Some(10), Some("a"), "A convention"),
            convention("me/web", "B-RULE", "b", "B convention"),
        ];
        let all = adoptable_conventions(&rows);
        let keys: Vec<(String, String)> = all
            .iter()
            .map(|c| (c.repo.clone(), c.rule_id.clone()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("me/api".to_string(), "A-RULE".to_string()),
                ("me/api".to_string(), "B-RULE".to_string()),
                ("me/web".to_string(), "B-RULE".to_string()),
            ],
            "stable (repo, rule) order"
        );
        assert_eq!(
            all[0].deviating_sites, 3,
            "the deviation-backed one carries its count"
        );
        assert_eq!(all[1].deviating_sites, 0, "the clean one reports none");
        assert_eq!(all[1].status_line(), "observed, no exceptions");
    }

    /// req_pass2_14: the same rule in two repos is TWO adoptions, because it is two
    /// `AGENTS.md` files. Adopting in one must not commit the other.
    #[test]
    fn req_pass2_14_destinations_are_per_repo() {
        let rows = vec![
            convention("me/api", "R-1", "a", "A"),
            convention("me/web", "R-1", "a", "A"),
        ];
        let all = adoptable_conventions(&rows);
        assert_eq!(convention_destinations(&all), vec!["me/api", "me/web"]);
        let mut model = AdoptionModel::new();
        model.set_adopted(&adoption_key("me/web", "R-1"), false);
        let api = CommitPlan::conventions(&all, &model, "me/api");
        let web = CommitPlan::conventions(&all, &model, "me/web");
        assert_eq!(api.adopted_count(), 1);
        assert_eq!(web.adopted_count(), 0);
        assert_eq!(web.skipped, 1);
    }

    // ── Opt-out ──────────────────────────────────────────────────────────────

    /// req_pass2_14 (Q4): adoption is OPT-OUT. An untouched convention is adopted; Skip is the
    /// gesture that changes something.
    #[test]
    fn req_pass2_14_adoption_is_opt_out_by_default() {
        let rows = vec![
            convention("me/api", "R-1", "a", "A"),
            convention("me/api", "R-2", "b", "B"),
            convention("me/api", "R-3", "c", "C"),
        ];
        let all = adoptable_conventions(&rows);
        let mut model = AdoptionModel::new();
        assert!(
            model.is_adopted(&adoption_key("me/api", "R-1")),
            "untouched = adopted"
        );
        assert_eq!(
            CommitPlan::conventions(&all, &model, "me/api").adopted_count(),
            3
        );

        model.set_adopted(&adoption_key("me/api", "R-2"), false);
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        assert_eq!(plan.adopted_count(), 2, "skip removes exactly one");
        assert_eq!(plan.skipped, 1);
        assert!(
            !plan.adopted.iter().any(|r| r.rule_id == "R-2"),
            "and it is the one that was skipped: {plan:?}"
        );

        model.toggle_adopted(&adoption_key("me/api", "R-2"));
        assert_eq!(
            CommitPlan::conventions(&all, &model, "me/api").adopted_count(),
            3,
            "toggling back restores it"
        );
    }

    // ── The unopened count ───────────────────────────────────────────────────

    /// req_pass2_14 (**PROOF THE UNOPENED COUNT IS REAL**): it tracks what was actually
    /// opened, one gesture at a time.
    ///
    /// The easy way to fake this number is to make it a constant, a ratio, or an alias for the
    /// adopted count. So this walks five conventions and opens them one at a time, asserting
    /// the count falls by exactly one on each open and never moves for any other reason — the
    /// shape no constant and no derivation-from-adoption can satisfy.
    #[test]
    fn req_pass2_14_the_unopened_count_tracks_what_was_actually_opened() {
        let rows: Vec<FindingView> = (1..=5)
            .map(|i| convention("me/api", &format!("R-{i}"), "opt", "Label"))
            .collect();
        let all = adoptable_conventions(&rows);
        let mut model = AdoptionModel::new();

        let plan = CommitPlan::conventions(&all, &model, "me/api");
        assert_eq!(plan.adopted_count(), 5);
        assert_eq!(plan.unopened_count(), 5, "nothing opened yet");

        for (i, expected) in (1..=5).zip([4, 3, 2, 1, 0]) {
            model.mark_opened(&adoption_key("me/api", &format!("R-{i}")));
            let plan = CommitPlan::conventions(&all, &model, "me/api");
            assert_eq!(
                plan.unopened_count(),
                expected,
                "after opening R-{i} exactly {expected} remain unopened: {plan:?}"
            );
            assert_eq!(
                plan.adopted_count(),
                5,
                "opening changes nothing about adoption"
            );
        }
        assert_eq!(
            CommitPlan::conventions(&all, &model, "me/api").unopened_warning(),
            None,
            "and with everything opened the warning is GONE, not a zero sentence"
        );
    }

    /// req_pass2_14: the unopened count names the RIGHT rules, not merely the right quantity.
    /// A count that happened to be numerically correct while listing the wrong rules would
    /// make the dialog's third exit ("review the unopened set") send the curator to the wrong
    /// place.
    #[test]
    fn req_pass2_14_the_unopened_set_names_the_rules_that_were_not_opened() {
        let rows = vec![
            convention("me/api", "OPENED-1", "a", "A"),
            convention("me/api", "UNREAD-1", "b", "B"),
            convention("me/api", "OPENED-2", "c", "C"),
            convention("me/api", "UNREAD-2", "d", "D"),
        ];
        let all = adoptable_conventions(&rows);
        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "OPENED-1"));
        model.mark_opened(&adoption_key("me/api", "OPENED-2"));
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        assert_eq!(plan.unopened_count(), 2);
        assert_eq!(
            plan.unopened_rule_ids(),
            vec!["UNREAD-1".to_string(), "UNREAD-2".to_string()],
            "the named set is the unopened one: {plan:?}"
        );
    }

    /// req_pass2_14: a SKIPPED convention is not in the unopened count, because it is not
    /// being written. The warning's subject is "what this commit writes without you having
    /// read it", so counting declined rules would inflate it and teach the curator to discount
    /// the number.
    #[test]
    fn req_pass2_14_a_skipped_convention_is_not_counted_as_unopened() {
        let rows = vec![
            convention("me/api", "R-1", "a", "A"),
            convention("me/api", "R-2", "b", "B"),
        ];
        let all = adoptable_conventions(&rows);
        let mut model = AdoptionModel::new();
        model.set_adopted(&adoption_key("me/api", "R-2"), false);
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        assert_eq!(plan.adopted_count(), 1);
        assert_eq!(plan.unopened_count(), 1, "only the one being written");
        assert_eq!(plan.unopened_rule_ids(), vec!["R-1".to_string()]);
    }

    /// req_pass2_14: marking opened is NOT derivable from adoption, from skipping, or from
    /// toggling. Only [`AdoptionModel::mark_opened`] writes the record.
    #[test]
    fn req_pass2_14_openness_is_not_a_side_effect_of_any_other_gesture() {
        let key = adoption_key("me/api", "R-1");
        let mut model = AdoptionModel::new();
        assert!(!model.was_opened(&key));
        model.set_adopted(&key, false);
        assert!(!model.was_opened(&key), "skipping is not reading");
        model.set_adopted(&key, true);
        assert!(!model.was_opened(&key), "nor is adopting");
        model.toggle_adopted(&key);
        model.toggle_adopted(&key);
        assert!(!model.was_opened(&key), "nor is toggling twice");
        model.set_recommendation(&key, Some("x".to_string()));
        assert!(!model.was_opened(&key), "nor is picking an option");
        model.mark_opened(&key);
        assert!(model.was_opened(&key), "only an explicit open does");
    }

    /// req_pass2_14: the approved warning copy, with the real number in it, and its absence
    /// when there is nothing to warn about.
    #[test]
    fn req_pass2_14_the_dialog_sentences_carry_the_real_numbers() {
        let rows: Vec<FindingView> = (1..=36)
            .map(|i| convention("me/api", &format!("R-{i}"), "opt", "Label"))
            .collect();
        let all = adoptable_conventions(&rows);
        let mut model = AdoptionModel::new();
        for i in 1..=11 {
            model.mark_opened(&adoption_key("me/api", &format!("R-{i}")));
        }
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        assert_eq!(
            plan.headline(),
            "You are writing 36 conventions into a client repository. This changes how future \
             AI agents working on that codebase will behave."
        );
        assert_eq!(
            plan.unopened_warning().expect("25 were not opened"),
            "25 conventions were adopted by default and not opened. Adoption is opt-out, so \
             these were selected for you."
        );
        assert!(
            plan.counts_line().contains("me/api"),
            "the destination is named"
        );
    }

    /// req_pass2_14: the sentences agree in number with one item.
    #[test]
    fn req_pass2_14_the_sentences_agree_in_number() {
        let rows = vec![convention("me/api", "R-1", "a", "A")];
        let all = adoptable_conventions(&rows);
        let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "me/api");
        assert!(
            plan.headline().contains("1 convention into"),
            "{}",
            plan.headline()
        );
        let warn = plan.unopened_warning().expect("one unopened");
        assert!(
            warn.starts_with("1 convention was adopted by default"),
            "{warn}"
        );
    }

    // ── The gate ─────────────────────────────────────────────────────────────

    /// req_pass2_14 (**PROOF THE GATE CANNOT BE BYPASSED**): only the destination repository
    /// name, typed, arms the commit.
    ///
    /// The gate is the easiest thing in this build to leave as decoration — a dialog that
    /// renders an input and then enables its button anyway. So this walks the near-misses a
    /// hurried curator actually produces, and each one must leave the commit SHUT.
    #[test]
    fn req_pass2_14_the_commit_gate_cannot_be_bypassed() {
        let rows = vec![
            convention("acme/payments-api", "R-1", "a", "A"),
            convention("acme/payments-web", "R-2", "b", "B"),
        ];
        let all = adoptable_conventions(&rows);
        let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "acme/payments-api");
        assert_eq!(
            plan.adopted_count(),
            1,
            "precondition: there is something to write"
        );

        for wrong in [
            "",                   // nothing typed
            "   ",                // whitespace only
            "acme/payments-web",  // THE OTHER REPO IN THE SAME SCAN — the dangerous one
            "payments-api",       // the repo without its owner
            "acme",               // the owner alone
            "acme/payments",      // a prefix
            "acme/payments-api2", // a superstring
            "acme/payments-api/", // a trailing slash
            "acme /payments-api", // interior whitespace
            "yes",                // the checkbox reflex
            "confirm",
            "AGENTS.md",
        ] {
            let gate = commit_gate(&plan, wrong);
            assert!(
                !gate.is_armed(),
                "typing {wrong:?} must NOT arm the commit (gate was {gate:?})"
            );
            assert!(
                gate.hint("acme/payments-api").is_some(),
                "and must say why: {wrong:?}"
            );
        }

        // Only the destination arms it — with surrounding whitespace and case forgiven,
        // because neither can be produced without reading the whole name.
        for right in [
            "acme/payments-api",
            "  acme/payments-api  ",
            "ACME/Payments-API",
        ] {
            assert!(
                commit_gate(&plan, right).is_armed(),
                "typing {right:?} must arm the commit"
            );
            assert_eq!(commit_gate(&plan, right).hint("acme/payments-api"), None);
        }
    }

    /// req_pass2_14: an EMPTY plan can never arm, however perfectly the destination is typed.
    /// A commit that writes nothing should not present itself as a commit.
    #[test]
    fn req_pass2_14_an_empty_plan_never_arms() {
        let plan = CommitPlan {
            destination_repo: "me/api".to_string(),
            noun: "conventions".to_string(),
            adopted: vec![],
            skipped: 3,
        };
        assert_eq!(commit_gate(&plan, "me/api"), CommitGate::NothingToCommit);
        assert!(!commit_gate(&plan, "me/api").is_armed());
    }

    /// req_pass2_14: the gate distinguishes "not yet typed" from "typed something wrong", so
    /// the dialog can prompt in the first case and object in the second.
    #[test]
    fn req_pass2_14_the_gate_distinguishes_untyped_from_wrong() {
        let rows = vec![convention("me/api", "R-1", "a", "A")];
        let all = adoptable_conventions(&rows);
        let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "me/api");
        assert_eq!(commit_gate(&plan, ""), CommitGate::AwaitingDestination);
        assert_eq!(commit_gate(&plan, "me/ap"), CommitGate::Mismatch);
        assert!(commit_gate(&plan, "me/ap")
            .hint("me/api")
            .expect("a hint")
            .contains("me/api"));
    }

    // ── The NoConvention pick (Q3) ───────────────────────────────────────────

    /// req_pass2_14 (Q3): a `NoConvention` option pick defaults to "leave as reported",
    /// produces the SAME [`AdoptedRule`] an adoption does when picked, and inherits the SAME
    /// typed gate — because it reaches the same `AGENTS.md` by the same path.
    #[test]
    fn req_pass2_14_a_no_convention_pick_is_a_gated_governance_act() {
        let rows = vec![split(
            "me/api",
            "ERROR-HANDLING-1",
            serde_json::json!([
                { "option_id": "result", "label": "Result types", "sites": 9 },
                { "option_id": "panic", "label": "Panics", "sites": null },
            ]),
        )];
        let all = split_rules(&rows);
        assert_eq!(all.len(), 1);
        assert_eq!(split_destinations(&all), vec!["me/api"]);

        let mut model = AdoptionModel::new();
        let plan = CommitPlan::recommendations(&all, &model, "me/api");
        assert!(plan.is_empty(), "DEFAULT is leave-as-reported: {plan:?}");
        assert_eq!(plan.skipped, 1, "and the untouched rule is accounted for");
        assert_eq!(commit_gate(&plan, "me/api"), CommitGate::NothingToCommit);

        model.set_recommendation(
            &adoption_key("me/api", "ERROR-HANDLING-1"),
            Some("result".into()),
        );
        let plan = CommitPlan::recommendations(&all, &model, "me/api");
        assert_eq!(plan.adopted_count(), 1);
        assert_eq!(plan.adopted[0].option_id, "result");
        assert_eq!(
            plan.adopted[0].label, "Result types",
            "the AUTHORED label, not the id"
        );
        assert_eq!(
            plan.unopened_count(),
            1,
            "and it carries the same openness honesty"
        );
        assert!(
            plan.headline().contains("1 recommendation into"),
            "{}",
            plan.headline()
        );

        // The same gate, with the same refusals.
        assert!(!commit_gate(&plan, "me/web").is_armed());
        assert!(!commit_gate(&plan, "").is_armed());
        assert!(commit_gate(&plan, "me/api").is_armed());

        // And clearing the pick returns it to leave-as-reported.
        model.set_recommendation(&adoption_key("me/api", "ERROR-HANDLING-1"), None);
        assert!(CommitPlan::recommendations(&all, &model, "me/api").is_empty());
    }

    /// req_pass2_14: an uncounted observed option renders as its bare label — the same honesty
    /// the panel already has, kept when the option becomes selectable.
    #[test]
    fn req_pass2_14_an_uncounted_option_never_acquires_a_fabricated_count() {
        let rows = vec![split(
            "me/api",
            "R-1",
            serde_json::json!([
                { "option_id": "a", "label": "Counted", "sites": 4 },
                { "option_id": "b", "label": "Uncounted", "sites": null },
            ]),
        )];
        let all = split_rules(&rows);
        assert_eq!(
            all[0].option_line("a").as_deref(),
            Some("Counted (4 sites)")
        );
        assert_eq!(all[0].option_line("b").as_deref(), Some("Uncounted"));
    }

    // ── The ruleset round trip ───────────────────────────────────────────────

    /// req_pass2_14 (**THE SILENT-ERASURE GUARD**): the commit is a read-modify-write of the
    /// whole `selections` array, so a field the mirror forgets is a field the commit deletes
    /// from every OTHER selection in the project.
    ///
    /// `applied_hash` / `applied_directive` are the rule-drift baseline. Losing them would
    /// make every rule in the project report as never-baselined after one convention commit —
    /// a corruption with no error message, discovered weeks later on the Rules screen.
    #[test]
    fn req_pass2_14_the_ruleset_round_trip_preserves_every_selection_field() {
        // The server's own JSON shape for a fully-populated selection.
        let wire = serde_json::json!({
            "selections": [{
                "rule_id": "EXISTING-1",
                "chosen_option": "old",
                "repos": ["me/web"],
                "applied_hash": "sha-abc",
                "applied_directive": "the directive as baselined",
                "applied_version": 3,
            }],
            "cross_repo": [],
            "process": [],
            "custom": [{ "name": "HOUSE-STYLE", "body": "...", "domain": "*", "repos": [] }],
        });
        let current: ProjectRulesetView =
            serde_json::from_value(wire).expect("the server shape deserializes");
        assert_eq!(
            current.selections[0].applied_hash.as_deref(),
            Some("sha-abc")
        );

        let merged = merge_adoptions(
            &current,
            "me/api",
            &[AdoptedRule {
                rule_id: "NEW-1".to_string(),
                option_id: "opt".to_string(),
                label: "Opt".to_string(),
                opened: true,
            }],
        );
        let sel = &merged.selections[0];
        assert_eq!(
            sel.rule_id, "EXISTING-1",
            "the untouched selection survives"
        );
        assert_eq!(
            sel.chosen_option.as_deref(),
            Some("old"),
            "its option is untouched"
        );
        assert_eq!(
            sel.repos,
            vec!["me/web".to_string()],
            "its repos are untouched"
        );
        assert_eq!(
            sel.applied_hash.as_deref(),
            Some("sha-abc"),
            "ITS DRIFT BASELINE SURVIVES the round trip"
        );
        assert_eq!(
            sel.applied_directive.as_deref(),
            Some("the directive as baselined")
        );
        assert_eq!(
            sel.applied_version,
            Some(3),
            "THE BASELINE VERSION SURVIVES the round trip too"
        );

        // And the POSTed body carries no `custom` key at all, so the server's `merge_custom`
        // sees an empty list and leaves the architect's custom rules exactly where they are.
        let body = serde_json::to_value(&merged).expect("serializes");
        assert!(
            body.get("custom").is_none(),
            "no `custom` key may be sent — the server preserves them only if we do not \
             overwrite them: {body}"
        );
        for key in ["selections", "cross_repo", "process"] {
            assert!(body.get(key).is_some(), "{key} must be sent; body={body}");
        }
    }

    /// req_pass2_14: an adoption for a rule the project ALREADY selects updates that selection
    /// in place and unions the destination into its repos — it never creates a duplicate.
    #[test]
    fn req_pass2_14_an_adoption_updates_an_existing_selection_in_place() {
        let current = ProjectRulesetView {
            selections: vec![RuleSelectionView {
                rule_id: "api-versioning-1".to_string(), // lowercase on purpose
                chosen_option: Some("path".to_string()),
                repos: vec!["me/web".to_string()],
                applied_hash: Some("h".to_string()),
                applied_directive: None,
                applied_version: None,
            }],
            ..Default::default()
        };
        let merged = merge_adoptions(
            &current,
            "me/api",
            &[AdoptedRule {
                rule_id: "API-VERSIONING-1".to_string(),
                option_id: "versioned".to_string(),
                label: "URI-versioned endpoints".to_string(),
                opened: false,
            }],
        );
        assert_eq!(merged.selections.len(), 1, "no duplicate: {merged:?}");
        assert_eq!(
            merged.selections[0].chosen_option.as_deref(),
            Some("versioned")
        );
        assert_eq!(
            merged.selections[0].repos,
            vec!["me/web".to_string(), "me/api".to_string()],
            "the destination is unioned in, the existing repo is kept"
        );
        assert_eq!(merged.selections[0].applied_hash.as_deref(), Some("h"));
    }

    /// req_pass2_14: a rule the project selects at CROSS-REPO or PROCESS scope is updated
    /// where it lives. Appending a second, repo-local selection for the same rule would make
    /// the project hold two contradictory positions on one rule.
    #[test]
    fn req_pass2_14_an_adoption_does_not_move_a_rule_between_scopes() {
        let current = ProjectRulesetView {
            cross_repo: vec![RuleSelectionView {
                rule_id: "CONTRACT-1".to_string(),
                chosen_option: Some("old".to_string()),
                repos: vec![],
                ..Default::default()
            }],
            process: vec![RuleSelectionView {
                rule_id: "COMMIT-FMT-1".to_string(),
                chosen_option: None,
                repos: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        let merged = merge_adoptions(
            &current,
            "me/api",
            &[
                AdoptedRule {
                    rule_id: "CONTRACT-1".to_string(),
                    option_id: "new".to_string(),
                    label: "New".to_string(),
                    opened: true,
                },
                AdoptedRule {
                    rule_id: "COMMIT-FMT-1".to_string(),
                    option_id: "conventional".to_string(),
                    label: "Conventional".to_string(),
                    opened: true,
                },
            ],
        );
        assert!(merged.selections.is_empty(), "nothing appended: {merged:?}");
        assert_eq!(merged.cross_repo[0].chosen_option.as_deref(), Some("new"));
        assert_eq!(
            merged.process[0].chosen_option.as_deref(),
            Some("conventional")
        );
    }

    /// req_pass2_14: adopting the same convention twice (a double-click, a retry after a
    /// network error) leaves exactly the state one adoption leaves. The commit has to be safe
    /// to repeat, because the UI cannot promise it will not be.
    #[test]
    fn req_pass2_14_committing_twice_is_the_same_as_committing_once() {
        let adopted = [AdoptedRule {
            rule_id: "R-1".to_string(),
            option_id: "opt".to_string(),
            label: "Opt".to_string(),
            opened: true,
        }];
        let once = merge_adoptions(&ProjectRulesetView::default(), "me/api", &adopted);
        let twice = merge_adoptions(&once, "me/api", &adopted);
        assert_eq!(once, twice, "idempotent: {once:?} vs {twice:?}");
        assert_eq!(once.selections.len(), 1);
        assert_eq!(once.selections[0].repos, vec!["me/api".to_string()]);
    }

    /// req_pass2_14: the merge NEVER drops a selection it was not asked about, at any scope.
    #[test]
    fn req_pass2_14_the_merge_never_drops_an_untouched_selection() {
        let current = ProjectRulesetView {
            selections: (1..=5)
                .map(|i| RuleSelectionView {
                    rule_id: format!("KEEP-{i}"),
                    ..Default::default()
                })
                .collect(),
            cross_repo: vec![RuleSelectionView {
                rule_id: "KEEP-X".to_string(),
                ..Default::default()
            }],
            process: vec![RuleSelectionView {
                rule_id: "KEEP-P".to_string(),
                ..Default::default()
            }],
        };
        let merged = merge_adoptions(
            &current,
            "me/api",
            &[AdoptedRule {
                rule_id: "BRAND-NEW".to_string(),
                option_id: "o".to_string(),
                label: "O".to_string(),
                opened: true,
            }],
        );
        let ids: Vec<&str> = merged
            .selections
            .iter()
            .map(|s| s.rule_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "KEEP-1",
                "KEEP-2",
                "KEEP-3",
                "KEEP-4",
                "KEEP-5",
                "BRAND-NEW"
            ]
        );
        assert_eq!(merged.cross_repo.len(), 1);
        assert_eq!(merged.process.len(), 1);
    }

    /// req_pass2_14: an empty adopted set is a no-op on the ruleset. The UI must never be able
    /// to rewrite a project's selections as a side effect of opening a dialog and cancelling.
    #[test]
    fn req_pass2_14_an_empty_commit_rewrites_nothing() {
        let current = ProjectRulesetView {
            selections: vec![RuleSelectionView {
                rule_id: "R-1".to_string(),
                chosen_option: Some("a".to_string()),
                repos: vec!["me/api".to_string()],
                applied_hash: Some("h".to_string()),
                applied_directive: Some("d".to_string()),
                applied_version: Some(2),
            }],
            ..Default::default()
        };
        assert_eq!(merge_adoptions(&current, "me/api", &[]), current);
    }
}
