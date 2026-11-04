//! The fix-specific stage: the prompt, the grounding and contradiction checks, the retry rounds and the pass that fills the fix text.

use super::*;

// ════════════════════════════════════════════════════════════════════════════════════
// FIX-SPECIFIC GENERATION + SELF-CHECK (P2, 2026-09-29)
// ════════════════════════════════════════════════════════════════════════════════════
//
// `Finding::fix_specific` used to be a stable `None` — `report_export::CuratedSiteJson`'s
// `fix_for_this_finding` field existed but nothing ever populated it (see that field's own
// doc comment before this pass). This section is the dedicated AI pass that fills it: a
// batched generation call over every non-dependency finding (modeled on `verify_findings`
// above — same `&dyn LlmPort`, same `UsageMeter`, same total-backstop timeout), followed by
// a bounded validate-and-regenerate loop so a fix that names an identifier absent from the
// finding's own evidence, or contradicts the finding's own `detail`, never reaches the
// client. A finding that still has no valid fix after every retry gets an honest
// needs-review marker instead of an empty/fabricated Fix block — see
// [`generate_fix_specifics`]'s own doc comment for the full contract.

/// How many EXTRA generation attempts a finding gets after its first fix fails the
/// self-check (identifier grounding or non-contradiction) — 2, per the design doc's "a small
/// N" bound. Each retry re-sends ONLY the still-failing findings, with a one-line note on
/// exactly what was wrong, so the model has a concrete correction to make rather than
/// guessing again blind.
pub(super) const MAX_FIX_REGENERATIONS: usize = 2;

/// Rank 10: the model the fix stage runs on when no fix model is set and the `fix_on_haiku`
/// efficiency measure is on (the default). Defined in `camerata-api-types` beside
/// [`DEFAULT_MODEL`] (the cockpit and the CLI print its name); re-exported here for the server.
pub use camerata_api_types::project::FIX_STAGE_HAIKU_MODEL;

/// Rank 10: the `output_config.effort` the fix call sends, on every model. The 5.x models think
/// adaptively at `medium` unless told otherwise; the fix task is bounded by its evidence and
/// re-checked, so `low` is the starting point (see `docs/TECHNICAL.md`, "Fix stage model and
/// effort"). Raise it only when the give-up rate or a fix-quality sample says the fixes need
/// more.
pub const FIX_STAGE_EFFORT: &str = camerata_ui_core::scan::FIX_EFFORT;

/// The model the fix stage runs on, in precedence order:
///
/// 1. `explicit`: the fix model the operator set (the project's fix step model, or
///    `camerata inspect --fix-model`). Blank counts as unset.
/// 2. With `fix_on_haiku` on: [`FIX_STAGE_HAIKU_MODEL`].
/// 3. With it off: `calibration_model.or(model)`, exactly the pre-rank-10 choice (`None` lets
///    the transport pick its default, as it always has).
///
/// The ONE place this is decided: the fix call and the spend-limit preflight
/// ([`crate::cost_cap::models_the_run_will_use`]) both come through here, so the model that is
/// checked for a price is the model that is called.
pub fn resolve_fix_model<'a>(
    explicit: Option<&'a str>,
    fix_on_haiku: bool,
    model: Option<&'a str>,
    calibration_model: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(explicit) = explicit.map(str::trim).filter(|m| !m.is_empty()) {
        return Some(explicit);
    }
    if fix_on_haiku {
        return Some(FIX_STAGE_HAIKU_MODEL);
    }
    calibration_model.or(model)
}

/// The effort the fix call asks for. ONE rule: [`FIX_STAGE_EFFORT`] whatever the fix model is,
/// because the effort belongs to the STAGE, not the model (Opus 5.5 and Haiku 5.5 both default
/// to `medium`, Sonnet 5.5 to `high`; a fix is one bounded sentence per finding at any of
/// them). The request carries it only to a model that accepts an effort setting
/// (`camerata_llm::model_registry::accepts_effort`), so an explicit pick of an older model keeps
/// the request it always had.
pub fn fix_effort() -> &'static str {
    FIX_STAGE_EFFORT
}

/// SYSTEM PROMPT for the fix-specific generation pass. Deliberately narrow: this pass does
/// NOT re-judge severity/confidence/effort (calibration already did that) and does NOT
/// explain the violation (the finding's own `detail` already does) — its ONLY job is a
/// concrete "change X in file Y" sentence grounded in the evidence it's given.
pub(crate) fn fix_specific_system_prompt() -> String {
    r#"You write ONE concrete, codebase-specific fix per finding for a paying client's audit
report. You are NOT re-explaining what is wrong — the finding's own detail already does that.
Your only job is telling the developer EXACTLY what to change, in THIS codebase.

Rules, every fix:
- Name the REAL file/symbol/column/function from the evidence given for that finding (its own
  path, snippet, detail, captured objects, and surrounding code). NEVER invent a name that
  isn't present anywhere in the evidence you were given for that finding: a name or file path
  in backticks must appear in that evidence, and so must every table, column, function, file
  or variable name inside a code snippet (ordinary words and universal names such as
  `Authorization`, `console.error` and try/catch/finally are fine). Each finding's block ends
  with a `Names you may use:` line: the identifiers and file names in its evidence. As a
  backticked name or inside a snippet, use only those, names you know exist elsewhere in this
  repository (another file, a column in the schema), ordinary words and the universal names; a
  name that exists nowhere in the repository is rejected. When the fix needs a NEW name, or you
  are not sure a name exists, describe it in plain words ("a new helper that returns the signed
  URL") instead of naming it in backticks or in a snippet.
- When the surrounding context shows the repo ALREADY has the correct pattern somewhere else,
  point to it by name ("use `safeInternalPath` from lib/redirect.ts, as
  app/auth/signout/route.ts does") instead of describing the pattern generically.
- 1-4 sentences. A short, repo-style code snippet is fine when it helps, but keep it tight;
  the snippet is yours to write, only the existing names it uses must come from the evidence.
- Say WHAT to change and WHERE — NEVER how this finding was detected. Do not mention
  Camerata, a scanner, a linter, a rule id, a regex, "the audit", or any detection mechanism.
- NEVER assert something about the codebase's current state that contradicts the finding's own
  detail (e.g. do not tell the client to enable/turn on something the detail already says is
  enabled/on/configured) — if the detail says a mitigation already exists, your fix must
  address what's actually still missing or broken, not redo what's already there.
- NEVER repeat a credential value in a fix, not even partly. Name the variable and the line
  ("move the key on line 3 of `.env` into your secrets manager"), not the key itself.
- If you cannot write a grounded, non-contradictory fix for a finding from the evidence given,
  omit it from your response entirely rather than guessing.

Return ONLY JSON, no prose:
{"fixes":[{"id":"<the finding's id, copied exactly>","fix":"..."}]}
One entry per finding you could ground a fix for, addressed by the id in square brackets that
opens its block. Omit any id you could not confidently ground — an omitted fix is far better
than an invented one. Every finding you are sent is a fault with something to change (a
statement that the code follows a convention is never sent to you), so an omitted id is asked
about once more in a smaller request, and then gets the rule's own generic fix."#
        .to_string()
        + crate::onboard::redact::MODEL_REDACTION_NOTICE
}

/// A short, line-numbered excerpt of `path`'s content in `files`, centered on `line`
/// (`radius` lines each side) — the "enough surrounding repo context to name real
/// identifiers" the design doc asks for, without re-sending the whole file. Empty when
/// `path` isn't found in `files` or `line` is 0 (a path-based, not line-based, finding) —
/// the prompt still has `snippet`/`detail`/`captures` in that case.
pub(super) fn fix_context_window(
    files: &[(String, String)],
    path: &str,
    line: usize,
    radius: usize,
) -> String {
    if line == 0 {
        return String::new();
    }
    let Some((_, content)) = files.iter().find(|(p, _)| p == path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let idx = line.saturating_sub(1).min(lines.len().saturating_sub(1));
    let start = idx.saturating_sub(radius);
    let end = (idx + radius + 1).min(lines.len());
    let mut out = String::new();
    for (i, l) in lines[start..end].iter().enumerate() {
        out.push_str(&format!("{}: {l}\n", start + i + 1));
    }
    out
}

/// The evidence blob a finding's generated fix is checked against — EXACTLY the union of
/// what the generation prompt showed the model for that finding (path/snippet/detail/
/// captures/surrounding code), so "grounded" means "grounded in what the model actually
/// saw," never a stricter or looser set. Shared by [`build_fix_specific_block`] (prompt) and
/// [`find_ungrounded_identifier`] (validation) so the two can never silently drift apart.
pub(super) fn fix_evidence_blob(f: &Finding, files: &[(String, String)]) -> String {
    let context = fix_context_window(files, &f.path, f.line, 8);
    let captures = object_captures(f)
        .map(|(_, v)| v.clone())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{} {} {} {} {}",
        f.path, f.snippet, f.detail, captures, context
    )
}

/// The most names a finding's `Names you may use:` line lists.
pub(super) const MAX_FIX_NAMES_LISTED: usize = 40;

/// The identifiers and file names of a finding's fix evidence (`evidence`, the blob
/// [`fix_evidence_blob`] builds, which is exactly what the grounding check looks a name up in):
/// every file-like word and every word shaped like a name somebody chose (`snake_case`,
/// `camelCase`, `PascalCase` with two capitals), in order of first appearance, without repeats.
/// The prompt prints it under `Names you may use:` so the model is told which names the check
/// will find; a name in backticks or in a snippet that is not in the evidence is rejected
/// ([`find_ungrounded_identifier`]), whether or not it is listed here (a plain word that the
/// evidence holds is as good as a listed one).
pub(super) fn fix_names_available(evidence: &str) -> Vec<String> {
    let path_re = regex::Regex::new(r"[A-Za-z0-9_.\-/]+\.[A-Za-z]{1,6}\b").expect("static regex");
    // Position of each candidate, so the list reads in the order the evidence does.
    let mut found: Vec<(usize, String)> = Vec::new();
    for m in path_re.find_iter(evidence) {
        let word = m.as_str().trim_matches(|c: char| matches!(c, '.' | ','));
        if is_file_like(word, false) {
            found.push((m.start(), word.to_string()));
        }
    }
    let word_re = regex::Regex::new(r"[A-Za-z0-9_]+").expect("static regex");
    for m in word_re.find_iter(evidence) {
        let part = m.as_str();
        if part.len() >= 3 && is_chosen_name(part) {
            found.push((m.start(), part.to_string()));
        }
    }
    found.sort_by_key(|(pos, _)| *pos);
    let mut out: Vec<String> = Vec::new();
    for (_, name) in found {
        if !out.contains(&name) {
            out.push(name);
        }
        if out.len() >= MAX_FIX_NAMES_LISTED {
            break;
        }
    }
    out
}

/// The "Captured objects" line of a finding's block, or nothing when it captured none.
pub(super) fn captured_objects_line(f: &Finding) -> String {
    if object_captures(f).next().is_none() {
        return String::new();
    }
    let caps = object_captures(f)
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Captured objects: {caps}\n")
}

/// Build the per-finding prompt body for a fix-generation round. `findings` are the
/// findings THIS round is asking about (batch-local order) and `ids` their opaque ids, parallel
/// to it (the reply addresses a fix by id, never by position); `feedback`, when present for a
/// batch-local position, is a one-line note on why that finding's PRIOR attempt was
/// rejected, appended so the model corrects the specific problem instead of guessing again.
pub(super) fn build_fix_specific_block(
    findings: &[&Finding],
    ids: &[String],
    files: &[(String, String)],
    corpus: Option<&camerata_rules::RuleSet>,
    feedback: &std::collections::HashMap<usize, String>,
    nonce: &crate::prompt_frame::FrameNonce,
) -> String {
    let mut out = String::new();
    for (i, f) in findings.iter().enumerate() {
        let id = &ids[i];
        out.push_str(&format!(
            "[{id}] rule {} — severity {}\n",
            f.rule_id, f.severity
        ));
        // Everything that derives from the client's files (location, quoted code, detail,
        // captured object names, the context window) sits inside one frame.
        let mut evidence = format!("Location: {}:{}\n", f.path, f.line);
        if !f.snippet.is_empty() {
            evidence.push_str(&format!(
                "Snippet: {}\n",
                camerata_secrets::redact_str(&f.snippet)
            ));
        }
        evidence.push_str(&format!(
            "Detail: {}\n",
            camerata_secrets::redact_str(&f.detail)
        ));
        evidence.push_str(&captured_objects_line(f));
        let context = fix_context_window(files, &f.path, f.line, 8);
        if !context.is_empty() {
            evidence.push_str(&format!("Surrounding code:\n{context}"));
        }
        // The names the grounding check will find, printed so the model is told what is checked.
        let names =
            fix_names_available(&camerata_secrets::redact_str(&fix_evidence_blob(f, files)));
        if !names.is_empty() {
            evidence.push_str(&format!(
                "Names you may use: {}\n",
                names
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push_str(&crate::prompt_frame::frame_untrusted(
            nonce,
            &format!("finding {id}"),
            &evidence,
        ));
        if let Some(rule) = corpus.and_then(|c| c.get_by_id(&f.rule_id)) {
            if let Some(opt) = rule.resolved_option(f.evaluated_option_id.as_deref()) {
                if let Some(rem) = opt.remediation.as_deref().filter(|s| !s.trim().is_empty()) {
                    out.push_str(&format!(
                        "This rule's generic remediation (inspiration only — your fix must be \
                         SPECIFIC to this finding, not a restatement of this): {rem}\n"
                    ));
                }
            }
        }
        if let Some(reason) = feedback.get(&i) {
            out.push_str(&format!(
                "Your previous attempt for this finding was rejected: {reason}. Write a \
                 corrected fix.\n"
            ));
        }
        out.push('\n');
    }
    out
}

/// Parse the `{"fixes":[{"id":"...","fix":"..."}]}` response into opaque id -> fix text.
/// Robust: unparseable/malformed JSON, a missing `fixes` array, or a blank `fix` string for an
/// entry all simply leave that id absent from the map — the caller treats an absent id exactly
/// like "the model couldn't ground this one" (never a panic, never a fabricated empty string).
/// An id that is not one of the batch's matches nothing: the caller looks up its own ids.
pub(super) fn parse_fix_specifics(raw: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some(json) = extract_json_object(raw) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    let Some(arr) = v["fixes"].as_array() else {
        return out;
    };
    for entry in arr {
        let Some(id) = entry["id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(text) = entry["fix"].as_str() else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        out.insert(id.to_string(), text.to_string());
    }
    out
}

/// Bare, non-code words a fix is allowed to name without them being present in the
/// finding's evidence — generic literals/placeholders, never a real identifier a client
/// could go looking for. Deliberately short: anything NOT on this list must be grounded.
pub(super) const GENERIC_FIX_TERMS: &[&str] =
    &["true", "false", "null", "none", "undefined", "todo"];

/// Names that exist in every codebase or runtime and so are never "invented" by a fix: a
/// fix may name them whether or not the finding's evidence happens to show them. Kept
/// short on purpose; a name that is merely common in the repo does not belong here. Matched
/// exactly (case-sensitive) against a backticked span or a path-like token.
pub(super) const UNIVERSAL_FIX_NAMES: &[&str] = &[
    // A framework or runtime written with its dot reads as a file to the path regex; an
    // abbreviation reads as one too; `void` is a keyword like `try`. None is a name a
    // repository could hold, so none is asked for.
    "Next.js",
    "Node.js",
    "React.js",
    "Vue.js",
    "Nuxt.js",
    "Express.js",
    "Deno.js",
    "e.g",
    "i.e",
    "void",
    "Authorization",
    "console.error",
    "console.log",
    "console.warn",
    "console.info",
    "process.exit",
    "process.env",
    "finally",
    "try",
    "catch",
    "devDependencies",
    "dependencies",
    "toString",
    "toISOString",
    "hasOwnProperty",
    "addEventListener",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "encodeURIComponent",
    "decodeURIComponent",
    "parseInt",
    "parseFloat",
    "localStorage",
    "sessionStorage",
    "preventDefault",
    "stopPropagation",
    "current_user",
    "session_user",
    "current_setting",
    "set_config",
    "gen_random_uuid",
];

/// File extensions that make a dotted token inside a code snippet a file name worth
/// grounding (`lib/x.ts`, `.env.local`) rather than a member access (`console.error`).
pub(super) const FIX_FILE_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "sql", "env", "md", "yml", "yaml", "toml",
    "py", "rs", "css", "scss", "html", "sh", "lock", "txt", "conf", "cfg", "ini", "xml", "local",
    "example",
];

/// Characters that separate the words of a code snippet.
pub(super) const SNIPPET_SEPARATORS: &str = "(){}[]<>=+*,;\"'|&!?`";

/// True when a backticked span is a code snippet (whitespace, call parentheses, brackets,
/// operators, quotes or separators in it) rather than one name. A fix quoting its own new
/// code is not in the evidence by construction, so such a span is not checked as one
/// identifier: only the names chosen inside it are (see [`snippet_names`]).
pub(super) fn is_code_snippet(span: &str) -> bool {
    span.chars()
        .any(|c| c.is_whitespace() || SNIPPET_SEPARATORS.contains(c))
}

/// Whether a dotted or slashed word in a snippet is a file name (`lib/x.ts`, `seed.sql`,
/// `.env.local`) rather than a member access (`console.error`, `req.json`). `followed_by_call`
/// is true when the word is immediately followed by `(`, which makes it a call.
pub(super) fn is_file_like(word: &str, followed_by_call: bool) -> bool {
    if followed_by_call {
        return false;
    }
    if word.contains('/') {
        return word.contains('.');
    }
    word.rsplit_once('.').is_some_and(|(stem, ext)| {
        FIX_FILE_EXTENSIONS.contains(&ext) && (ext != "env" || stem.is_empty())
    })
}

/// The tokens of a code snippet worth grounding: any file name in it, any schema-qualified
/// table or environment variable it names (see [`qualified_object`]), and any word of 3 or
/// more characters shaped like a name somebody chose (see [`is_chosen_name`]). Ordinary
/// words, language keywords, SQL keywords and single-word builtins (`err`, `catch`, `table`,
/// `Promise`) are never asked for: the snippet is the fix's own new code.
pub(super) fn snippet_names(span: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = span;
    while !rest.is_empty() {
        let skip = rest
            .find(|c: char| !(c.is_whitespace() || SNIPPET_SEPARATORS.contains(c)))
            .unwrap_or(rest.len());
        rest = &rest[skip..];
        if rest.is_empty() {
            break;
        }
        let len = rest
            .find(|c: char| c.is_whitespace() || SNIPPET_SEPARATORS.contains(c))
            .unwrap_or(rest.len());
        let (raw, after) = rest.split_at(len);
        rest = after;
        let word = raw.trim_matches(|c: char| matches!(c, ':' | '-'));
        let followed_by_call = after.starts_with('(');
        let word_trimmed = word.trim_end_matches('.');
        // D3: a standard JavaScript, Web or Node name (`Number.isSafeInteger`, `crypto.randomUUID`)
        // is the platform's, not a name the repository chose.
        if crate::fix_standard_names::is_standard_js_name(word_trimmed) {
            continue;
        }
        if is_file_like(word_trimmed, followed_by_call) {
            out.push(word_trimmed.to_string());
            continue;
        }
        // `public.documents`, `storage.objects`: a schema-qualified name is a table (or, when
        // called, a function) somebody chose, even as one lowercase word; `process.env.SECRET`
        // is an environment variable somebody chose, even with no underscore. Neither shape is
        // caught by `is_chosen_name`, so the object name is asked for here. A qualified CALL
        // (`auth.uid()`) is a builtin or an existing function, not a table.
        if let Some(obj) = qualified_object(word_trimmed, followed_by_call) {
            if obj.len() >= 3 {
                out.push(obj.to_string());
            }
        }
        for tok in word.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if tok.len() >= 3
                && is_chosen_name(tok)
                && !crate::fix_standard_names::is_standard_js_name(tok)
            {
                out.push(tok.to_string());
            }
        }
    }
    out
}

/// Database schemas whose qualified names (`public.documents`) name a table or function.
pub(super) const SCHEMA_QUALIFIERS: &[&str] =
    &["public", "auth", "internal", "storage", "extensions"];

/// The object a qualified word in a snippet names: the table of `schema.table` (not when the
/// word is called, which makes it a function such as `auth.uid()`), or the variable of
/// `process.env.NAME`. `None` for any other word.
pub(super) fn qualified_object(word: &str, followed_by_call: bool) -> Option<&str> {
    if let Some(name) = word.strip_prefix("process.env.") {
        return Some(name).filter(|n| !n.is_empty());
    }
    if followed_by_call {
        return None;
    }
    let (schema, name) = word.split_once('.')?;
    if SCHEMA_QUALIFIERS.contains(&schema) && !name.is_empty() && !name.contains('.') {
        Some(name)
    } else {
        None
    }
}

/// Whether a token looks like a name its author chose (`snake_case`, `camelCase`, or a
/// `PascalCase` word with two or more capitals) rather than a plain word, keyword or acronym.
pub(super) fn is_chosen_name(tok: &str) -> bool {
    if tok.contains('_') {
        // `NODE_ENV`, `owner_id`, `member_contacts`; not a bare run of underscores or digits.
        return tok.chars().any(|c| c.is_ascii_alphabetic());
    }
    let first_upper = tok.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    let uppers = tok.chars().filter(|c| c.is_ascii_uppercase()).count();
    let has_lower = tok.chars().any(|c| c.is_ascii_lowercase());
    has_lower
        && if first_upper {
            uppers >= 2
        } else {
            uppers >= 1
        }
}

/// **Identifier-grounding validator (pure).** Every concrete identifier a generated `fix`
/// names — a `` `backticked` `` symbol/column, or a bare file-path-like token
/// (`lib/redirect.ts`, `schema.sql`, `package.json`) — must appear verbatim in `evidence`
/// (the finding's own path/snippet/detail/captures/surrounding code — see
/// [`fix_evidence_blob`]) or be one of [`GENERIC_FIX_TERMS`] / [`UNIVERSAL_FIX_NAMES`].
/// A backticked span that is a code snippet (see [`is_code_snippet`]) is the fix's own new
/// code, so only the names chosen inside it (and any file path in it) are checked, never the
/// whole span. Returns the FIRST offending identifier found, so the caller can hand the
/// model a concrete, specific correction ("you named X, which isn't in the evidence") rather
/// than a vague rejection. Returns `None` when every named identifier is grounded (including
/// a fix that names none at all — pure prose is never rejected on this axis).
#[cfg(test)]
pub(crate) fn find_ungrounded_identifier(fix: &str, evidence: &str) -> Option<String> {
    find_ungrounded_identifier_in(fix, evidence, None)
}

/// The names a repository holds: the paths of its files and every identifier its files and paths
/// spell. A name a fix uses that sits outside the finding's own evidence window but exists in the
/// repository (`actor_id` two screens down, `tsconfig.json`, `.env.example`, a helper in another
/// file) is a real name, not an invented one; a name found nowhere in the repository still is
/// ([`find_ungrounded_identifier_in`]). Built once per repo from the file set the fix stage
/// already holds.
pub(crate) struct RepoNames<'a> {
    files: &'a [(String, String)],
    /// Every identifier-shaped word of a path or a file's content (`[A-Za-z0-9_]+`).
    words: std::collections::HashSet<&'a str>,
}

impl<'a> RepoNames<'a> {
    pub(crate) fn new(files: &'a [(String, String)]) -> Self {
        let word_re = regex::Regex::new(r"[A-Za-z0-9_]+").expect("static regex");
        let mut words: std::collections::HashSet<&'a str> = std::collections::HashSet::new();
        for (path, content) in files {
            for text in [path.as_str(), content.as_str()] {
                words.extend(word_re.find_iter(text).map(|m| m.as_str()));
            }
        }
        Self { files, words }
    }

    /// Whether the repository holds `name`. A path-like name (a slash, or a file extension)
    /// is a file or directory of the set (exact, a trailing part of a path, a directory on it,
    /// or a file's base name), or is written in some file's content; any other name is known
    /// when every identifier word in it is one the repository spells.
    pub(crate) fn knows(&self, name: &str) -> bool {
        let name = name.trim_start_matches("./").trim_end_matches('/');
        if name.is_empty() {
            return false;
        }
        if name.contains('/') || is_file_like(name, false) {
            // `env.example` is how a fix trimmed of its dot names `.env.example`: a dotfile's
            // base name is read with or without its leading dot.
            let on_a_path = self.files.iter().any(|(p, _)| {
                let base = p.rsplit('/').next().unwrap_or(p);
                p == name
                    || p.ends_with(&format!("/{name}"))
                    || p.starts_with(&format!("{name}/"))
                    || p.contains(&format!("/{name}/"))
                    || base.strip_prefix('.') == Some(name)
            });
            return on_a_path || self.files.iter().any(|(_, c)| c.contains(name));
        }
        let mut parts = name
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|p| !p.is_empty())
            .peekable();
        parts.peek().is_some() && parts.all(|p| self.words.contains(p))
    }
}

/// [`find_ungrounded_identifier`] with the repository to look a name up in when the finding's
/// evidence does not hold it. `None` is the evidence alone.
pub(crate) fn find_ungrounded_identifier_in(
    fix: &str,
    evidence: &str,
    repo: Option<&RepoNames<'_>>,
) -> Option<String> {
    let backtick_re = regex::Regex::new(r"`([^`]+)`").expect("static regex");
    let path_re = regex::Regex::new(r"[A-Za-z0-9_.\-/]+\.[A-Za-z]{1,6}\b").expect("static regex");

    let mut candidates: Vec<String> = Vec::new();
    // The fix with its snippet spans blanked: a `console.error` or `process.exit` inside a
    // snippet is code, not a file name, and the snippet's own file names are collected from
    // the snippet itself.
    let mut prose = fix.to_string();
    for cap in backtick_re.captures_iter(fix) {
        let span = &cap[1];
        if is_code_snippet(span) {
            candidates.extend(snippet_names(span));
            prose = prose.replace(&cap[0], " ");
        } else {
            candidates.push(span.to_string());
        }
    }
    for m in path_re.find_iter(&prose) {
        candidates.push(m.as_str().to_string());
    }

    for ident in candidates {
        let trimmed = ident.trim_matches(|c: char| matches!(c, '.' | ',' | ')' | '(' | ';' | ':'));
        if trimmed.is_empty() {
            continue;
        }
        if GENERIC_FIX_TERMS.contains(&trimmed.to_ascii_lowercase().as_str())
            || UNIVERSAL_FIX_NAMES.contains(&trimmed)
            || crate::fix_standard_names::is_standard_js_name(trimmed)
            || crate::fix_standard_names::is_known_eslint_rule(trimmed)
        {
            continue;
        }
        if !evidence.contains(trimmed) && !repo.is_some_and(|r| r.knows(trimmed)) {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Common words that precede an "already X" phrase without being the SUBJECT of it (e.g.
/// "this file is already enabled" — "file" isn't what's enabled). Excluded from
/// [`fix_contradicts_detail`]'s subject extraction so the heuristic keys on the actual
/// setting/capability name ("strict mode", "RLS", "verify_jwt"), not filler.
pub(super) const CONTRADICTION_STOPWORDS: &[&str] = &[
    "this", "that", "with", "have", "already", "file", "code", "function", "which", "it", "the",
];

/// Phrases in a finding's `detail` asserting some setting/capability is ALREADY in a given
/// state — the premise half of the known "told to fix what's already fixed" contradiction
/// class (the design doc's strict-mode example generalized: this fires for ANY already-on
/// setting, not just strict mode — RLS, `verify_jwt`, 2FA, ...).
pub(super) const ALREADY_MARKERS: &[&str] = &[
    "already enabled",
    "already on",
    "already true",
    "already active",
    "already configured",
    "already turned on",
    "is already enabled",
    "currently enabled",
];

/// Verbs that, in a fix, instruct turning a setting ON — the instruction half of the
/// contradiction class. Deliberately narrow (no generic "add"/"set", which fire on
/// unrelated, legitimate fixes) so this stays a targeted check, not a broad false-positive
/// generator.
pub(super) const ENABLE_VERBS: &[&str] = &[
    "enable",
    "turn on",
    "turning on",
    "switch on",
    "re-enable",
    "reenable",
    "activate",
];

/// **Non-contradiction validator (pure, rule-based).** Detects the known "told to do what's
/// already done" contradiction class: `detail` asserts a setting/capability is ALREADY in
/// some state (`ALREADY_MARKERS`), and `fix` instructs enabling/turning on ([`ENABLE_VERBS`])
/// that SAME setting — the strict-mode-already-on example generalized to any subject, not
/// hardcoded to "strict". Returns `Some(reason)` (fed back to the model verbatim as the
/// regeneration note) on a hit, `None` otherwise. This is a TARGETED rule check for one known
/// contradiction shape, not a general consistency oracle — see this module's banner comment
/// for why a rule-based check satisfies the design doc's self-check requirement on its own.
pub(crate) fn fix_contradicts_detail(fix: &str, detail: &str) -> Option<String> {
    let detail_lower = detail.to_ascii_lowercase();
    let fix_lower = fix.to_ascii_lowercase();

    for marker in ALREADY_MARKERS {
        let Some(pos) = detail_lower.find(marker) else {
            continue;
        };
        let before = &detail_lower[..pos];
        let subject_tokens: Vec<&str> = before
            .split_whitespace()
            .rev()
            .take(4)
            .flat_map(|w| w.split(|c: char| !c.is_alphanumeric()))
            .filter(|t| t.len() >= 4 && !CONTRADICTION_STOPWORDS.contains(t))
            .collect();
        if subject_tokens.is_empty() {
            continue;
        }
        for verb in ENABLE_VERBS {
            let Some(vpos) = fix_lower.find(verb) else {
                continue;
            };
            let window_end = (vpos + verb.len() + 60).min(fix_lower.len());
            let window = &fix_lower[vpos..window_end];
            if subject_tokens.iter().any(|t| window.contains(t)) {
                return Some(format!(
                    "the finding's detail says \"{marker}\" but the fix tells the client to \
                     {verb} the same thing — that's already done, address what's ACTUALLY \
                     still wrong"
                ));
            }
        }
    }
    None
}

/// Phrases that describe HOW Camerata found a finding rather than WHAT to change — the
/// no-methodology-leak invariant (FIX-1, carried into `fix_specific`: see
/// `report_export::resolve_fix`'s doc comment for the original instance of this rule over
/// the rule-authored `fix` field). A client-facing fix must never read like a scanner's
/// self-description.
pub(super) const METHODOLOGY_LEAK_PHRASES: &[&str] = &[
    "camerata",
    "this rule detects",
    "this rule checks",
    "this rule flags",
    "the scanner",
    "the audit tool",
    "our scan",
    "our tool",
    "the scan flagged",
    "detected by",
    "regex-scan",
    "regex scan",
    "static analysis",
    "the gate",
    "flagged this finding",
    "the audit found",
];

/// **Methodology-leak guard (pure).** Returns the first offending phrase found in `fix`
/// (case-insensitive), or `None` when the fix describes only the remediation.
pub(crate) fn fix_leaks_methodology(fix: &str) -> Option<String> {
    let lower = fix.to_ascii_lowercase();
    METHODOLOGY_LEAK_PHRASES
        .iter()
        .find(|p| lower.contains(*p))
        .map(|p| p.to_string())
}

/// Why each of `pending` still has no fix, as the failed part records it: the place of the
/// finding and the reason its latest reply was turned down. A finding with no reason on file
/// never got a reply back (the call failed in transport, or the spend limit stopped it).
pub(super) fn fix_rejections(
    findings: &[Finding],
    pending: &[usize],
    feedback: &std::collections::HashMap<usize, String>,
    ledger: &FixLedger,
) -> Vec<String> {
    pending
        .iter()
        .map(|&i| {
            let f = &findings[i];
            let earlier = feedback.get(&i).map(String::as_str);
            let why = match (ledger.last_miss.get(&i), earlier) {
                (Some(FixMiss::TransportFailed), Some(e)) => {
                    format!(
                        "the last call failed in transport (an earlier reply was turned down: {e})"
                    )
                }
                (Some(FixMiss::TransportFailed), None) => {
                    "the last call failed in transport".to_string()
                }
                (Some(FixMiss::CapStopped), _) => "the spend limit stopped the call".to_string(),
                (None, Some(e)) => e.to_string(),
                (None, None) => "no reply came back for it".to_string(),
            };
            format!("{}:{} {}: {why}", f.path, f.line, f.rule_id)
        })
        .collect()
}

/// One check a model-written fix failed. A fix is wording a reviewer reads, so a failure is a
/// review flag on the row ([`crate::review_flags`]) after one re-ask, never a held run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FixFailure {
    /// The fix names an identifier found neither in the evidence nor in the repository.
    Ungrounded(String),
    /// The fix tells the client to do what the finding's detail says is already done (the reason
    /// is the check's own sentence).
    Contradiction(String),
    /// The fix describes how the finding was detected (the phrase it used).
    MethodologyLeak(String),
}

impl FixFailure {
    fn flag(&self) -> &'static str {
        match self {
            Self::Ungrounded(_) => crate::review_flags::FIX_UNGROUNDED,
            Self::Contradiction(_) => crate::review_flags::CONTRADICTION,
            Self::MethodologyLeak(_) => crate::review_flags::METHODOLOGY_LEAK,
        }
    }

    /// The reason in words, for the saved reply and the failure report.
    fn reason(&self) -> String {
        match self {
            Self::Ungrounded(bad) => format!(
                "named `{bad}`, which is not in the finding's evidence or anywhere in the \
                 repository"
            ),
            Self::Contradiction(reason) => reason.clone(),
            Self::MethodologyLeak(leak) => {
                format!("described detection methodology (\"{leak}\")")
            }
        }
    }

    /// The one-line correction the re-ask carries.
    fn feedback(&self) -> String {
        match self {
            Self::Ungrounded(bad) => format!(
                "you named `{bad}`, which appears neither in this finding's evidence \
                 nor anywhere in the repository; keep your fix and its code snippet, \
                 but use only names that exist, or describe a new name in plain words"
            ),
            Self::Contradiction(reason) => reason.clone(),
            Self::MethodologyLeak(leak) => format!(
                "you described detection methodology (\"{leak}\") \u{2014} describe only \
                 the fix, never how it was found"
            ),
        }
    }
}

/// Every check `text` fails, in the order the checks run (grounding, contradiction, methodology).
pub(super) fn fix_failures(
    text: &str,
    finding: &Finding,
    evidence: &str,
    repo_names: &RepoNames<'_>,
) -> Vec<FixFailure> {
    let mut out = Vec::new();
    if let Some(bad) = find_ungrounded_identifier_in(text, evidence, Some(repo_names)) {
        out.push(FixFailure::Ungrounded(bad));
    }
    if let Some(reason) = fix_contradicts_detail(text, &finding.detail) {
        out.push(FixFailure::Contradiction(reason));
    }
    if let Some(leak) = fix_leaks_methodology(text) {
        out.push(FixFailure::MethodologyLeak(leak));
    }
    out
}

/// A fix reply that failed at least one check, kept until the finding is settled.
#[derive(Debug, Clone)]
pub(super) struct FixReply {
    /// The redacted fix text.
    text: String,
    failures: Vec<FixFailure>,
}

impl FixReply {
    /// Whether this reply is worse than the rule's own authored fix: it tells the client to redo
    /// what is done, or it describes how the finding was detected. An unverified name is not: the
    /// replay of 40 such rejections found no real error among them.
    fn worse_than_authored(&self) -> bool {
        self.failures.iter().any(|f| {
            matches!(
                f,
                FixFailure::Contradiction(_) | FixFailure::MethodologyLeak(_)
            )
        })
    }

    fn reasons(&self) -> String {
        self.failures
            .iter()
            .map(FixFailure::reason)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The fix replies that failed a check, by finding. A finding is in here from its first failed
/// reply until a later reply passes or it is settled ([`settle_fix_wording`]). Its presence in a
/// call's batch is what makes that call the finding's one re-ask.
#[derive(Debug, Default)]
pub(super) struct FixWording {
    replies: std::collections::HashMap<usize, Vec<FixReply>>,
}

/// Decide what a finding whose fix replies all failed a check ships, write it onto `findings[idx]`
/// and return the review flags that record the failure. Never blocks the run.
///
/// The better reply ships: one that is not worse than the authored fix beats one that is, then
/// the one that failed fewer checks, then the later (it had the correction). When the best reply
/// is still worse than the authored fix ([`FixReply::worse_than_authored`]) and the rule has one,
/// the authored fix ships instead, marked as a standard fix. A reply with only an unverified name
/// ships as it is: it is the tailored text, and a reviewer reads the flag.
pub(super) fn settle_fix_wording(
    findings: &mut [Finding],
    idx: usize,
    replies: &[FixReply],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<String> {
    let Some(best) = replies
        .iter()
        .rev()
        .min_by_key(|r| (r.worse_than_authored(), r.failures.len()))
    else {
        return Vec::new();
    };
    let flags: Vec<String> = best.failures.iter().map(|f| f.flag().to_string()).collect();
    if best.worse_than_authored() {
        if let Some(authored) =
            crate::report_export::resolve_fix(&findings[idx].rule_id, corpus, &findings[idx], None)
        {
            findings[idx].fix_specific = Some(camerata_secrets::redact_str(&authored).into_owned());
            crate::report_export::mark_standard_fix(&mut findings[idx], &best.reasons());
            return flags;
        }
    }
    findings[idx].fix_specific = Some(best.text.clone());
    flags
}

/// Why a finding never got a response from the fix model on its last attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FixMiss {
    /// The spend limit refused the batch the finding was in; no call was made.
    CapStopped,
    /// The call was made and failed in transport (timeout, HTTP error, dropped stream).
    TransportFailed,
}

/// What happened to each finding sent through the fix rounds, kept per finding so the give-up
/// rate counts only what it means: a finding the model ANSWERED (at least one call returned and
/// was parsed) and still produced no fix that passed the self-check. A finding that was never
/// answered (the spend limit stopped it, or every call failed in transport) is not a model
/// give-up and is reported as its own figure. Filled by [`run_fix_specifics_round`], summed into
/// a [`crate::stage_telemetry::FixOutcome`] by [`FixLedger::outcome`].
#[derive(Default)]
pub(super) struct FixLedger {
    /// Findings a call was actually made for, at least once.
    sent: std::collections::HashSet<usize>,
    /// Findings whose call returned a response that was parsed, at least once.
    answered: std::collections::HashSet<usize>,
    /// For a finding that missed its latest attempt without an answer: why.
    last_miss: std::collections::HashMap<usize, FixMiss>,
    /// Findings whose latest attempt failed in transport (see [`second_try_allowed`]): the only
    /// ones the end-of-run second try sends again.
    retry_at_end: std::collections::HashSet<usize>,
}

impl FixLedger {
    fn miss(&mut self, batch: &[usize], why: FixMiss) {
        for &i in batch {
            self.last_miss.insert(i, why);
        }
    }

    /// A call is about to be made for `batch`: how each of its findings last failed starts afresh.
    fn sent(&mut self, batch: &[usize]) {
        self.sent.extend(batch.iter().copied());
        for i in batch {
            self.retry_at_end.remove(i);
        }
    }

    /// The call for `batch` failed in transport and the run may still send it again at the end.
    fn retryable_at_end(&mut self, batch: &[usize]) {
        self.retry_at_end.extend(batch.iter().copied());
    }

    /// The members of `pending` the end-of-run second try covers (transport failures only).
    fn split_for_second_try(&self, pending: &[usize]) -> (Vec<usize>, Vec<usize>) {
        pending
            .iter()
            .copied()
            .partition(|i| self.retry_at_end.contains(i))
    }

    /// A call for `batch` returned a response.
    fn answered(&mut self, batch: &[usize]) {
        self.answered.extend(batch.iter().copied());
        for i in batch {
            self.last_miss.remove(i);
        }
    }

    /// The run's tally once the rounds are over, given the findings still `pending` (no fix that
    /// passed the self-check) and how many the pre-filter handled with no model call.
    fn outcome(
        &self,
        pending: &[usize],
        authored_without_model: usize,
    ) -> crate::stage_telemetry::FixOutcome {
        let mut out = crate::stage_telemetry::FixOutcome {
            sent_to_model: self.sent.len() as u64,
            answered: self.answered.len() as u64,
            authored_without_model: authored_without_model as u64,
            ..Default::default()
        };
        for i in pending {
            if self.answered.contains(i) {
                out.gave_up += 1;
            } else if self.last_miss.get(i) == Some(&FixMiss::CapStopped) {
                out.cap_stopped += 1;
            } else {
                out.transport_failed += 1;
            }
        }
        out
    }
}

/// One batch of a fix round: the findings it asks about and what its reply is read against.
pub(super) struct FixBatch {
    batch: Vec<usize>,
    ids: Vec<String>,
    /// Per finding: it already has a fix turned down by a check, so this call is its one re-ask.
    reasking: Vec<bool>,
    request_key: String,
    req: LlmRequest,
}

/// The calls of one fix round in the shape the shared batch runner takes: one item per batch,
/// named by the round and the site keys of the findings it covers. The stage's own state (the
/// findings, the ledger, the wording replies) is carried here, so reading a reply is the same
/// code whether the reply is fresh or replayed from a checkpoint.
pub(super) struct FixRound<'a> {
    llm: &'a dyn LlmPort,
    meter: Option<&'a UsageMeter>,
    files: &'a [(String, String)],
    repo_names: &'a RepoNames<'a>,
    findings: &'a mut [Finding],
    feedback: &'a mut std::collections::HashMap<usize, String>,
    wording: &'a mut FixWording,
    ledger: &'a mut FixLedger,
    batches: std::collections::BTreeMap<String, FixBatch>,
    order: Vec<String>,
    still_pending: Vec<usize>,
}

impl stage_batch::StageItems for FixRound<'_> {
    type Id = String;

    fn items(&self) -> Vec<(String, LlmRequest)> {
        self.order
            .iter()
            .map(|k| (k.clone(), self.batches[k].req.clone()))
            .collect()
    }

    // The stage runs its own rounds and its own end-of-run second try.
    fn resends(&self) -> bool {
        false
    }

    fn absorb(
        &mut self,
        key: &String,
        resp: crate::llm::LlmResponse,
    ) -> Result<(), stage_batch::ItemReject> {
        let Some(bctx) = self.batches.get(key) else {
            return Err(stage_batch::ItemReject::new(
                "the reply names a batch this round did not ask",
            ));
        };
        let batch = &bctx.batch[..];
        let ids = &bctx.ids;
        let reasking = &bctx.reasking;
        let request_key = &bctx.request_key;
        let findings = &mut *self.findings;
        let files = self.files;
        let repo_names = self.repo_names;
        let meter = self.meter;
        let llm = self.llm;
        let still_pending = &mut self.still_pending;
        let feedback = &mut *self.feedback;
        let wording = &mut *self.wording;
        let ledger = &mut *self.ledger;
        ledger.answered(batch);
        let parsed = parse_fix_specifics(&resp.text);
        // Why each fix of THIS reply was turned down, named by place; the reply carries them.
        let mut turned_down: Vec<String> = Vec::new();
        let place = |f: &Finding, why: &str| format!("{}:{} {}: {why}", f.path, f.line, f.rule_id);

        for (pos, &orig_idx) in batch.iter().enumerate() {
            // A finding that failed a check and has now had its one re-ask is settled from the
            // replies it has; everything else is asked again next round.
            let settles_here = reasking[pos] && wording.replies.contains_key(&orig_idx);
            let Some(text) = parsed.get(&ids[pos]) else {
                feedback
                    .entry(orig_idx)
                    .or_insert_with(|| "you did not return a fix for this finding".to_string());
                turned_down.push(place(
                    &findings[orig_idx],
                    "the reply held no fix for this finding",
                ));
                if !settles_here {
                    still_pending.push(orig_idx);
                }
                continue;
            };
            // The model's fix text is client-visible and often repeats the offending value:
            // redact it BEFORE the self-check, so grounding compares redacted to redacted.
            let redacted_text = camerata_secrets::redact_str(text).into_owned();
            let evidence = fix_evidence_blob(&findings[orig_idx], files);
            let failures = fix_failures(&redacted_text, &findings[orig_idx], &evidence, repo_names);
            if failures.is_empty() {
                findings[orig_idx].fix_specific = Some(redacted_text);
                wording.replies.remove(&orig_idx);
                // A fix that passes carries no fix flag from an earlier attempt or run.
                if !crate::review_flags::flags_of(&findings[orig_idx]).is_empty() {
                    crate::review_flags::set_stage_flags(
                        &mut findings[orig_idx],
                        crate::review_flags::FlagStage::Fix,
                        Vec::new(),
                    );
                }
                continue;
            }
            let reply = FixReply {
                text: redacted_text,
                failures,
            };
            turned_down.push(place(&findings[orig_idx], &reply.reasons()));
            feedback.insert(orig_idx, reply.failures[0].feedback());
            wording.replies.entry(orig_idx).or_default().push(reply);
            if !settles_here {
                still_pending.push(orig_idx);
            }
        }
        // The reply records the reasons, and only this reply. It is set aside only when EVERY
        // fix in it was turned down: a reply with an accepted fix stays good, so a resume
        // replays that fix instead of paying for the whole batch again.
        if !turned_down.is_empty() {
            let why = format!(
                "{} of {} fix(es) turned down: {}",
                turned_down.len(),
                batch.len(),
                turned_down.join("; ")
            );
            llm.note_rejected_reply(request_key, &why, turned_down.len() == batch.len());
        }
        // The fixes this call produced that were thrown away (missing, ungrounded,
        // contradicting, leaking) are tokens spent for nothing: say so in the telemetry, pro rata
        // for a batch that was only partly rejected.
        let rejected = turned_down.len();
        if rejected > 0 {
            if let Some(m) = meter {
                m.record_wasted_share(
                    Stage::FixGeneration,
                    &resp,
                    rejected as f64 / batch.len() as f64,
                );
            }
        }
        Ok(())
    }

    fn retry(&self, _key: &String, _reject: &stage_batch::ItemReject) -> Option<LlmRequest> {
        None
    }
}

/// One ROUND of the fix-specific generation loop (C6-B3.4 extracted this out of
/// [`generate_fix_specifics`] so a normal `FIX_BATCH_SIZE`-batched round and the final
/// single-finding retry share IDENTICAL request-building, sending, and self-check logic -
/// the only axis that varies between callers is `batch_size`). The batches are sent through the
/// shared batch runner (several at once); `round` names the round for the checkpoint, so a
/// restart replays a round's finished batches and asks the rest. Mutates `findings[idx]
/// .fix_specific` in place for anything that validates; returns the subset of `pending` that
/// still needs another round. A transport/timeout failure on one chunk re-queues only THAT
/// chunk's findings, never the whole still-pending set.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_fix_specifics_round(
    llm: &dyn LlmPort,
    repo: &str,
    system: &str,
    nonce: &crate::prompt_frame::FrameNonce,
    fix_model: Option<&str>,
    meter: Option<&UsageMeter>,
    corpus: Option<&camerata_rules::RuleSet>,
    files: &[(String, String)],
    repo_names: &RepoNames<'_>,
    findings: &mut [Finding],
    feedback: &mut std::collections::HashMap<usize, String>,
    wording: &mut FixWording,
    pending: &[usize],
    batch_size: usize,
    round: &str,
    ledger: &mut FixLedger,
    trail: &mut StageTrail,
    second_try: bool,
) -> Vec<usize> {
    let site_keys = crate::inspections::site_keys_of(findings);
    let mut batches = std::collections::BTreeMap::new();
    let mut order = Vec::new();
    // Bounded chunks (see `FIX_BATCH_SIZE`'s doc comment) - each is an independent call, so a
    // failure/timeout in ONE chunk only re-queues ITS findings for the next attempt, never the
    // whole still-pending set. `pending.chunks(..)` yields exactly one chunk (the whole set) when
    // `batch_size >= pending.len()`, so the request/self-check shape below is unchanged from
    // before this fix in that case.
    for batch in pending.chunks(batch_size.max(1)) {
        // A finding that already has a fix turned down by a check is asked again in this call:
        // its one re-ask, whose answer (or silence) settles it.
        let reasking: Vec<bool> = batch
            .iter()
            .map(|i| wording.replies.contains_key(i))
            .collect();
        // An opaque id per finding of the batch: the reply addresses a fix by it, not by position.
        let mut ids: Vec<String> = Vec::with_capacity(batch.len());
        for &i in batch {
            let id = detail_regen::opaque_id(repo, i, &findings[i], &ids);
            ids.push(id);
        }
        let prompt = {
            let refs: Vec<&Finding> = batch.iter().map(|&i| &findings[i]).collect();
            let local_feedback: std::collections::HashMap<usize, String> = batch
                .iter()
                .enumerate()
                .filter_map(|(pos, orig)| feedback.get(orig).cloned().map(|r| (pos, r)))
                .collect();
            format!(
                "Repository: {repo}\n\nWrite a concrete fix for each finding below:\n\n{}",
                build_fix_specific_block(&refs, &ids, files, corpus, &local_feedback, nonce)
            )
        };
        let req = with_stage_effort(
            LlmRequest::new(prompt)
                .with_system(system.to_string())
                .with_max_tokens(fix_max_tokens(batch.len()))
                .with_response_schema(reply_schemas::fix_reply_schema(&ids))
                .with_model(stage_model(fix_model)),
            Stage::FixGeneration,
        );
        let request_key = crate::run_store::journal::request_key(&req);
        let key = crate::checkpoint::batch_key(round, batch.iter().map(|&i| site_keys[i].as_str()));
        order.push(key.clone());
        batches.insert(
            key,
            FixBatch {
                batch: batch.to_vec(),
                ids,
                reasking,
                request_key,
                req,
            },
        );
    }
    let mut stage = FixRound {
        llm,
        meter,
        files,
        repo_names,
        findings,
        feedback,
        wording,
        ledger,
        batches,
        order,
        still_pending: Vec::new(),
    };
    let checkpoint = crate::checkpoint::stage_checkpoint(&format!("fix_{repo}"));
    let model = stage_model(fix_model);
    let ctx = stage_batch::StageCtx {
        repo,
        model: &model,
        realtime: llm,
        batch: None,
        meter,
        job: None,
        checkpoint: checkpoint.as_ref(),
        settings: stage_batch::StageSettings::default(),
    };
    let outcome = stage_batch::run_stage(
        Stage::FixGeneration,
        &mut stage,
        stage_batch::StageMode::Parallel {
            max_in_flight: PARALLEL_CONCURRENCY,
        },
        &ctx,
    )
    .await;
    // What the runner could not get a reply for: those findings stay pending. The spend limit
    // and the outage stop record themselves on the meter; any other failure is a transport
    // failure the end-of-run second try may send again.
    let mut still_pending = std::mem::take(&mut stage.still_pending);
    let outage = meter.is_some_and(UsageMeter::outage_stopped);
    // A batch the spend limit or the outage stop refused, or the job cancelled, was never sent.
    let never_sent: std::collections::BTreeSet<&String> = outcome
        .failed
        .iter()
        .filter(|f| matches!(f.class, FailureClass::SpendLimit | FailureClass::OutageStop))
        .map(|f| &f.id)
        .chain(outcome.cancelled.iter().flat_map(|c| c.not_sent.iter()))
        .collect();
    for (key, bctx) in &stage.batches {
        if !never_sent.contains(key) {
            stage.ledger.sent(&bctx.batch);
        }
    }
    for f in &outcome.failed {
        let Some(bctx) = stage.batches.get(&f.id) else {
            continue;
        };
        still_pending.extend(bctx.batch.iter().copied());
        if matches!(f.class, FailureClass::SpendLimit | FailureClass::OutageStop) {
            stage.ledger.miss(&bctx.batch, FixMiss::CapStopped);
            continue;
        }
        let attempts: Vec<AttemptRecord> = f
            .attempts
            .iter()
            .cloned()
            .map(|mut r| {
                r.second_try = second_try;
                r
            })
            .collect();
        trail.note_failed_item(f.class, &f.reason, &attempts);
        if f.class.is_retried_at_end() && !outage {
            stage.ledger.retryable_at_end(&bctx.batch);
        }
        stage.ledger.miss(&bctx.batch, FixMiss::TransportFailed);
    }
    if let Some(cancelled) = &outcome.cancelled {
        for id in &cancelled.not_sent {
            if let Some(bctx) = stage.batches.get(id) {
                still_pending.extend(bctx.batch.iter().copied());
            }
        }
    }
    still_pending.sort_unstable();
    still_pending.dedup();
    still_pending
}

/// Whether `f` has code around it to ground a fix in: the same window the generation prompt would
/// show the model ([`fix_context_window`]). Empty for line 0 and for a path that is not among the
/// scanned files, which is exactly where [`find_ungrounded_identifier`] has only the snippet,
/// detail and captures to check a fix against.
pub(super) fn fix_has_surrounding_code(f: &Finding, files: &[(String, String)]) -> bool {
    !fix_context_window(files, &f.path, f.line, 8).is_empty()
}

/// Give every finding in `indices` that has no surrounding code AND whose rule carries an authored
/// remediation that remediation (instantiated against the finding, redacted), without asking the
/// model. Returns the indices still needing a model-generated fix, and how many were handled.
/// A finding with no authored remediation is left in the returned set.
pub(super) fn apply_authored_remediation_without_model(
    findings: &mut [Finding],
    indices: Vec<usize>,
    files: &[(String, String)],
    corpus: Option<&camerata_rules::RuleSet>,
) -> (Vec<usize>, usize) {
    let mut remaining = Vec::with_capacity(indices.len());
    let mut handled = 0usize;
    for idx in indices {
        if !fix_has_surrounding_code(&findings[idx], files) {
            if let Some(fallback) = crate::report_export::resolve_fix(
                &findings[idx].rule_id,
                corpus,
                &findings[idx],
                None,
            ) {
                findings[idx].fix_specific =
                    Some(camerata_secrets::redact_str(&fallback).into_owned());
                crate::report_export::mark_standard_fix_source(&mut findings[idx]);
                handled += 1;
                continue;
            }
        }
        remaining.push(idx);
    }
    (remaining, handled)
}

/// Run the P2 fix-specific generation pass: for every non-dependency finding in `findings`,
/// generate a codebase-specific `fix_specific` (see [`fix_specific_system_prompt`]), then
/// validate it (identifier grounding + non-contradiction + no-methodology-leak) and
/// regenerate up to [`MAX_FIX_REGENERATIONS`] times for anything that fails. A finding whose
/// fix STILL doesn't pass after every batched retry gets ONE more solo attempt (C6-B3.4,
/// `run_fix_specifics_round` with `batch_size = 1`) — a smaller, single-finding prompt is a
/// cheap reliability win on top of the batched retries, since it strips away any
/// cross-finding context (a batch-mate's evidence, a batch-mate's rejected fix) that may have
/// been confusing the model or tripping the self-check on THIS finding specifically.
///
/// A finding that STILL doesn't pass after that keeps `fix_specific` at `None` from the
/// MODEL's own attempt — but C6-B3 means that is no longer the end of the story: this
/// function now instantiates the RULE's own authored remediation against the finding's own
/// `path`/`captures` (via [`crate::report_export::resolve_fix`], the exact machinery the
/// generic `fix` field already uses) and writes THAT into `fix_specific` instead, so the
/// finding's PRIMARY fix line is still concrete and finding-specific — never a silent `None`,
/// never the model's fabricated/rejected text. Only a finding whose rule has no authored
/// remediation at all (no corpus, no corpus entry, or a blank `remediation` field) keeps
/// `fix_specific` as `None` — genuinely nothing to instantiate. Every give-up (whether or not
/// the instantiated fallback covered it) is recorded into the returned `Vec<FailedPass>` so
/// the report export can disclose the degradation (count included) rather than shipping
/// silently — see [`FailedPass`] and W6.
///
/// This is a PIPELINE failure (the model never produced a validated, codebase-specific fix),
/// not a confidence judgement about the finding itself, so it must NEVER set `needs_review`,
/// NEVER hedge, and NEVER tag `detail` with pipeline-state text (C3-1b,
/// the cycle-2 queue-hardening notes) — the ONLY thing that sets
/// `needs_review`/`confidence = "needs-review"` is a calibration doubt verdict
/// (`apply_verdicts`). A row that STILL has no fix from ANY source (no model-generated
/// specific, no instantiated authored remediation) is never dropped and never refused here —
/// [`crate::report_export::is_action_row_missing_a_fix`] re-routes exactly that case to
/// `"held"` at report-export time, with its own disclosed reason.
///
/// Modeled on [`verify_findings`] above: same `&dyn LlmPort` seam, same [`UsageMeter`]
/// folding, same streamed-call + bounded-chunk shape (`FIX_BATCH_SIZE`, in the
/// "AGGREGATE-PASS TUNING" section). Dependency-audit findings (`DEP_AUDIT_RULE_ID`) are
/// skipped — they're carved into their own §7 lane and never flow through
/// `resolve_fix`/`CuratedSiteJson` either (see that carve-out's doc comment in
/// `report_export.rs`).
pub async fn generate_fix_specifics(
    llm: &dyn LlmPort,
    repo: &str,
    mut findings: Vec<Finding>,
    files: &[(String, String)],
    fix_model: Option<&str>,
    meter: Option<&UsageMeter>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> (Vec<Finding>, Vec<FailedPass>) {
    // A finding that already carries its codebase-specific fix keeps it: the only findings that
    // arrive with one are those a delta retest carried forward from the inspection for a file
    // that has not changed since, and regenerating would spend tokens re-sending that file's
    // code to the model to reproduce a fix that is still correct.
    let indices: Vec<usize> = findings
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.rule_id != crate::dep_audit::DEP_AUDIT_RULE_ID
                && f.fix_specific.is_none()
                // D2: a row that states a convention or an assessment gap (an informational
                // kind) has no defect to fix. The prompt tells the model to omit a fix it
                // cannot ground, so the model's correct omission here used to count as a
                // failure, be asked again in every round, and end as a failed part.
                && !f.kind.as_ref().is_some_and(|k| k.is_informational())
        })
        .map(|(i, _)| i)
        .collect();
    if indices.is_empty() {
        return (findings, Vec::new());
    }
    // The fix prompt carries eight raw lines either side of each finding: redact them.
    let model_files = crate::onboard::redact::files_for_model(files);
    let files = model_files.as_slice();
    // The names the repository holds, for grounding a name the finding's own window lacks.
    let repo_names = RepoNames::new(files);

    // PRE-FILTER (no model call). A finding with no surrounding code to read (line 0, or its file
    // is not among the scanned files) was the bulk of the fix retries that were paid for and
    // thrown away: on the hearth sweep 54 of about 125 findings exhausted every retry and 50 of
    // them fell back to the authored remediation anyway. For those the rule's own authored
    // remediation, instantiated against the finding's captures, is where they were always going
    // to end up, so go there directly. Only when that fallback actually exists: skipping the model
    // for a rule with no authored remediation would leave the row with no fix at all.
    let (indices, authored_directly) = if efficiency_of(meter).fix_prefilter {
        apply_authored_remediation_without_model(&mut findings, indices, files, corpus)
    } else {
        (indices, 0)
    };
    if authored_directly > 0 {
        eprintln!(
            "[camerata-server] fix-specific generation: {authored_directly} finding(s) in {repo} \
             have no surrounding code to ground a model fix in; used the rule's authored \
             remediation with no model call."
        );
    }
    // Telemetry: what became of each finding is tallied per finding (`FixLedger`) and recorded
    // once after the last round. When nothing is left for the model, only the pre-filter's count.
    if indices.is_empty() {
        if let Some(m) = meter {
            m.record_fix_outcome(FixLedger::default().outcome(&[], authored_directly));
        }
        return (findings, Vec::new());
    }
    let mut ledger = FixLedger::default();
    let mut trail = StageTrail::default();

    // One delimiter token for every round, regeneration rounds included (see `crate::prompt_frame`),
    // drawn so that it occurs in none of the evidence or files it will frame.
    let evidence = finding_evidence_texts(&findings);
    let mut avoid: Vec<&str> = evidence.iter().map(String::as_str).collect();
    avoid.extend(files.iter().flat_map(|(p, c)| [p.as_str(), c.as_str()]));
    let nonce = crate::prompt_frame::FrameNonce::generate_avoiding(&avoid);
    let system = crate::prompt_frame::with_untrusted_clause(fix_specific_system_prompt(), &nonce);
    // The subset of `indices` still needing a valid fix this round; shrinks as findings
    // pass their self-check or exhaust retries. Original-index -> rejection reason, fed
    // back into the NEXT round's prompt for that finding only.
    let mut pending = indices;
    let mut feedback: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut wording = FixWording::default();

    for attempt in 0..=MAX_FIX_REGENERATIONS {
        if pending.is_empty() {
            break;
        }
        pending = run_fix_specifics_round(
            llm,
            repo,
            &system,
            &nonce,
            fix_model,
            meter,
            corpus,
            files,
            &repo_names,
            &mut findings,
            &mut feedback,
            &mut wording,
            &pending,
            FIX_BATCH_SIZE,
            &format!("r{attempt}"),
            &mut ledger,
            &mut trail,
            false,
        )
        .await;
    }

    // C6-B3.4: one FINAL, cheap-reliability pass over whatever's still pending — SOLO
    // (batch_size = 1) rather than batched. See this function's doc comment for why a
    // smaller prompt is worth one extra call per still-pending finding.
    if !pending.is_empty() {
        pending = run_fix_specifics_round(
            llm,
            repo,
            &system,
            &nonce,
            fix_model,
            meter,
            corpus,
            files,
            &repo_names,
            &mut findings,
            &mut feedback,
            &mut wording,
            &pending,
            1,
            "solo",
            &mut ledger,
            &mut trail,
            false,
        )
        .await;
    }

    // END-OF-RUN SECOND TRY, for transport trouble only. A finding whose last attempt could not
    // reach the provider gets one more solo try, when the provider has had the whole stage to
    // recover, through the same admission and spend checks. A finding the model answered and the
    // self-check turned down is not sent again: the rounds already changed the request for it
    // (a feedback line, then a batch of one), and the same solo request a second time pays twice
    // for the same answer. A run that has stopped sends nothing. Only a part that is still
    // without a fix after this is a give-up (and so a failed part, which locks a client export).
    // A finding with a fix turned down by a check is settled below, not unusable.
    let unsettled: Vec<usize> = pending
        .iter()
        .copied()
        .filter(|i| !wording.replies.contains_key(i))
        .collect();
    if !unsettled.is_empty() {
        trail.note_unusable(
            "no usable fix was returned after the stage's own rounds",
            false,
            fix_rejections(&findings, &unsettled, &feedback, &ledger),
        );
    }
    let (retry, left_alone) = ledger.split_for_second_try(&pending);
    if !retry.is_empty() && !meter.is_some_and(UsageMeter::outage_stopped) {
        let still = run_fix_specifics_round(
            llm,
            repo,
            &system,
            &nonce,
            fix_model,
            meter,
            corpus,
            files,
            &repo_names,
            &mut findings,
            &mut feedback,
            &mut wording,
            &retry,
            1,
            "second",
            &mut ledger,
            &mut trail,
            true,
        )
        .await;
        if !still.is_empty() {
            trail.note_unusable(
                "no usable fix was returned on the second try",
                true,
                fix_rejections(&findings, &still, &feedback, &ledger),
            );
        }
        pending = left_alone;
        pending.extend(still);
        pending.sort_unstable();
    }

    // C6-B3: a give-up is never silent and never leaves the finding with no fix line when a
    // fix is available from ANY source. Instantiate the rule's own authored remediation
    // against THIS finding's own path/captures — the identical machinery that already backs
    // the generic `fix` field — and write it into `fix_specific`, so the finding's PRIMARY
    // fix line is concrete and finding-specific even though the model itself never produced
    // one. A finding whose rule carries no authored remediation at all keeps `fix_specific`
    // as `None`; `report_export::is_action_row_missing_a_fix` is what re-routes THAT case to
    // `"held"` rather than shipping an action-bucket row with no fix text anywhere.
    // WORDING NEVER BLOCKS (M4). A finding with a fix that failed a check is settled from the
    // replies it has: the better one ships and the failure is recorded on the row as a review
    // flag. That holds whether its one re-ask was answered, left out, refused by the spend limit
    // or lost to transport: the stage ran on it, and a reviewer reads the flag. Only a finding
    // that never got a reply stays pending, to the authored fix or a failed part as before.
    let mut to_settle: Vec<usize> = wording.replies.keys().copied().collect();
    to_settle.sort_unstable();
    for idx in to_settle {
        let Some(replies) = wording.replies.remove(&idx) else {
            continue;
        };
        let flags = settle_fix_wording(&mut findings, idx, &replies, corpus);
        let recorded = crate::review_flags::set_stage_flags(
            &mut findings[idx],
            crate::review_flags::FlagStage::Fix,
            flags,
        );
        if let Some(m) = meter {
            m.record_review_flags(crate::review_flags::FlagStage::Fix, &recorded);
        }
        pending.retain(|i| *i != idx);
    }
    if let Some(m) = meter {
        m.record_fix_outcome(ledger.outcome(&pending, authored_directly));
    }
    let mut failed_passes = Vec::new();
    // D8: a finding the model ANSWERED and the checks turned down in every round is not an
    // incomplete part: the fix stage ran, and the rule's own authored fix ships as a Standard
    // fix. Only work that never ran (the call failed in transport, or the spend limit or an
    // outage stopped it) stays a failed part, and so stays incomplete and resumable. A finding
    // with no authored fix to ship has no Standard fix either, so it stays a failed part too.
    //
    // "Rejected every round" is read from the ledger as: the model answered, and the LAST attempt
    // was an answer (a later call that failed in transport or was stopped by the limit leaves a
    // `last_miss`). A finding answered once and then lost did not get its rounds.
    //
    // A finding that stays a failed part gets NO fix written into `fix_specific`: the scan cache
    // carries a finding forward, and the next inspection's fix stage skips one that already has a
    // fix, so a pre-filled one would be labelled a tailored fix and never be asked of the model.
    // The export still shows the rule's own authored fix for it, as a Standard fix.
    let mut unfinished: Vec<usize> = Vec::new();
    let mut with_authored_fix = 0usize;
    let mut standard = 0usize;
    for &idx in &pending {
        let rejected_every_round =
            ledger.answered.contains(&idx) && !ledger.last_miss.contains_key(&idx);
        let fallback =
            crate::report_export::resolve_fix(&findings[idx].rule_id, corpus, &findings[idx], None);
        if rejected_every_round {
            if let Some(fallback) = fallback {
                findings[idx].fix_specific =
                    Some(camerata_secrets::redact_str(&fallback).into_owned());
                let why = fix_rejections(&findings, &[idx], &feedback, &ledger);
                crate::report_export::mark_standard_fix(&mut findings[idx], &why.join(" "));
                standard += 1;
                continue;
            }
        } else if fallback.is_some() {
            with_authored_fix += 1;
        }
        unfinished.push(idx);
    }
    if standard > 0 {
        eprintln!(
            "[camerata-server] fix-specific generation: {standard} finding(s) in {repo} had no \
             model-written fix pass the checks after {MAX_FIX_REGENERATIONS} batched retries \
             plus one single-finding retry; each ships the rule's own authored remediation, \
             instantiated against its own identifiers, as a standard fix. The run is complete."
        );
    }
    if !unfinished.is_empty() {
        let gave_up = unfinished.len();
        let instantiated = with_authored_fix;
        let uncovered = gave_up - instantiated;
        eprintln!(
            "[camerata-server] fix-specific generation did not finish for {gave_up} finding(s) \
             in {repo}; the part is incomplete."
        );
        failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: "fix-specific generation".to_string(),
            reason: format!(
                "per-finding fix generation gave up on {gave_up} finding(s) this run after \
                 {MAX_FIX_REGENERATIONS} batched retries plus one single-finding retry; \
                 {instantiated} have the rule's own authored fix to show as a Standard fix \
                 once the part completes, and {uncovered} have no authored fix (they would \
                 be held for manual review rather than shipped with no fix at all)"
            ),
            files_not_reviewed: 0,
            class: trail.class(),
            files: Vec::new(),
            attempts: trail.attempts.clone(),
            scope_note: false,
        });
    }

    (findings, failed_passes)
}
