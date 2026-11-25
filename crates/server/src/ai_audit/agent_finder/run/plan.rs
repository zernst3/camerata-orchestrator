//! The figures of `camerata inspect --plan-only`: what each piece's room would hold, with no
//! model call. The rooms are built in memory by the same functions the run uses and dropped at
//! once; nothing is written and only sizes are kept, never file text.

use super::super::{api_agent, brief_text, checklist, pieces, room::Room, PieceLimits};
use super::piece::{estimate_usd, lines_of, read_pages_of};
use crate::scan_routing::AuditRule;

/// The sizes of one piece's room and the limits it would run under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PieceFigures {
    pub repo: String,
    /// The unit the piece belongs to (a path, never file text).
    pub unit: String,
    pub files: usize,
    pub lines: usize,
    /// Bytes of the piece's own files.
    pub bytes: usize,
    pub context_files: usize,
    /// Bytes of the context files the room adds beside the piece.
    pub context_bytes: usize,
    /// Bytes of the checklist file the room serves.
    pub checklist_bytes: usize,
    /// Estimated input of the first request (system text, tools and brief; the room's files are
    /// read later, by tool calls), the output reserve NOT included.
    pub first_request_tokens: usize,
    /// The output reserve the context guard counts on top of a request: one turn's output limit.
    pub output_reserve_tokens: usize,
    /// A LOWER BOUND on the largest request of the piece, the output reserve included: the conversation after the model has
    /// read every file of the room whole through the Read tool (numbered lines, framed and
    /// JSON-escaped as the loop sends them), measured by the context guard's own arithmetic, so it
    /// compares with [`Self::guard_tokens`]. The model's replies, writes and other tool calls are
    /// not in it.
    pub peak_tokens: usize,
    /// The most a request may reach before the loop stops the piece (the context guard).
    pub guard_tokens: usize,
    pub turn_limit: usize,
}

impl PieceFigures {
    /// True when reading the whole room would pass the context guard.
    #[must_use]
    pub fn over_guard(&self) -> bool {
        self.peak_tokens > self.guard_tokens
    }
}

/// The figures of every piece of one repository: the same cut, rooms, checklists and limits
/// [`super::find_repo`] uses, over the same redacted files. `model` is the finder's model.
#[must_use]
pub fn piece_figures(
    repo: &str,
    files: &[(String, String)],
    selected: &[AuditRule],
    corpus: &camerata_rules::RuleSet,
    force_pieces: Option<usize>,
    applies_to_routing: bool,
    model: &str,
) -> Vec<PieceFigures> {
    let mut model_files = crate::onboard::redact::files_for_model(files);
    model_files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut selected: Vec<AuditRule> = selected.to_vec();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    let units = pieces::units_for(repo, &model_files);
    let planned = pieces::plan_pieces(&units, &model_files, pieces::TARGET_BYTES, force_pieces);
    let guard_tokens = api_agent::context_guard_tokens(model);
    let mut out = Vec::new();
    for piece in planned {
        let pairs: Vec<(String, String)> = model_files
            .iter()
            .filter(|(p, _)| piece.files.contains(p))
            .cloned()
            .collect();
        let list = checklist::build(&selected, corpus, &pairs, applies_to_routing);
        let rendered = list.render();
        let room = Room::build(&piece, &model_files, &rendered).room;
        let lines = lines_of(&model_files, &piece.files);
        let limits = PieceLimits::for_piece_reading(
            piece.files.len(),
            read_pages_of(&model_files, &piece.files),
            estimate_usd(lines),
        );
        let held = room.file_pairs();
        let size_of = |paths: &[String]| -> usize {
            held.iter()
                .filter(|(p, _)| paths.contains(p))
                .map(|(_, t)| t.len())
                .sum()
        };
        let (first, peak) = api_agent::request_sizes(
            model,
            &room,
            brief_text::PIECE_BRIEF,
            limits.max_turn_tokens,
        );
        out.push(PieceFigures {
            repo: repo.to_string(),
            unit: piece.unit.clone(),
            files: piece.files.len(),
            lines,
            bytes: size_of(room.piece_files()),
            context_files: room.context_files().len(),
            context_bytes: size_of(room.context_files()),
            checklist_bytes: rendered.len(),
            first_request_tokens: first.saturating_sub(limits.max_turn_tokens as usize),
            output_reserve_tokens: limits.max_turn_tokens as usize,
            peak_tokens: peak,
            guard_tokens,
            turn_limit: limits.max_turns,
        });
    }
    out
}
