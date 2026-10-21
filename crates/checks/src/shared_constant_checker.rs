//! `SharedConstantChecker`: the deterministic detector for `ARCH-SHARED-CONSTANT-1` ("a domain
//! value has one named home"). The algorithm and its false-positive traps are specified in
//! `docs/design/2026-10-21_w29-detector-implementation.md` ("ARCH-SHARED-CONSTANT-1: the
//! algorithm and its traps"); this module implements that version.
//!
//! Registered in [`crate::arch_checker::all_checkers`] together with the corpus rule
//! `crates/rules/principles/universal/arch-shared-constant-1.toml` (`universal` domain).
//!
//! # The definition is deliberately narrow
//!
//! The rule's value is "`X` is written as a bare literal in N files". The whole difficulty is
//! deciding, deterministically, which `X` are *domain* values and which are the protocol, the
//! framework, the styling or the test data. Owner ruling, 2026-10-21 (recorded in
//! `docs/OVERNIGHT_DECISIONS.md`, DECISIONS MADE): only four shapes qualify, and widening
//! happens one shape at a time, each with its own safe twins.
//!
//! - **S1** a separated lowercase token (`orders-high`, `user:signup`, `uploads/avatars`),
//! - **S2** a route template (`/api/v1/orders`, `/orders/:id`),
//! - **S3** a multi-word SCREAMING_SNAKE token (`ORDER_CREATED`),
//! - **N1** a decimal rate strictly between 0 and 1, never 0.5 / 0.25 / 0.75, and only where
//!   every bare copy is an operand of arithmetic (`total * 0.12`).
//!
//! Dotted keys, snake_case strings, single words and every integer are out of scope: no
//! deterministic guard separates their domain uses from their protocol, schema and styling uses.
//!
//! # How it decides
//!
//! 1. Every source file that survives the file-level exclusions (tests, config, migrations,
//!    scripts, generated code, declaration files, committed build output) is read with
//!    [`crate::extract::literals`]. Each literal that has one of the four shapes becomes an
//!    occurrence, grouped by `(language family, kind, value)`. A TypeScript client and a Python
//!    server writing the same route share a contract, not a constant, so a group never mixes
//!    families.
//! 2. **Value-level disqualifiers** remove a value entirely when ANY of its occurrences shows
//!    it is something else: an i18n key, a class name, an environment variable name, a typed
//!    enum member, an import specifier, an HTTP protocol token, a logger name, a query-builder
//!    fragment, a DOM event name. One occurrence is enough, because the same text in one place
//!    being a translation key means the other places cannot be told apart from it.
//! 3. **Occurrence-level exclusions** drop single occurrences (an object key, a subscript, an
//!    attribute that is not a route, a CSS property's value) while the value survives.
//! 4. A **home** is an occurrence that is the initializer of a module-level constant (through
//!    objects, arrays and `as const`). Everything else is a **bare** copy; a file that holds a
//!    home contributes no bare copies of that value.
//! 5. A value is flagged when it has no home and is bare in at least two files, or has a home
//!    and is bare in at least one other file. Two homes with no bare copy is not a finding in
//!    v1 (owner ruling, same date).
//!
//! # Documented limits (all false-negative, by design)
//!
//! See section 6 of the design doc. In short: dotted, snake_case, single-word and integer
//! values are never flagged; values inside Rust macro bodies, interpolated strings and
//! concatenations are invisible; cross-language repeats are not findings; a value touched once
//! by an i18n, class-name, environment, logger or header call is excluded everywhere.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`]: the checker contract forbids emitting `low`. The corpus rule
//! declares `max_severity = "low"`, which the report export clamps down to.

// TEMPORARY while the checker is built up over several commits; removed when `check` uses these.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_MEDIUM};
use crate::extract::{self, LiteralCtx, LiteralKind, SourceLang};

pub const RULE_SHARED_CONSTANT: &str = "ARCH-SHARED-CONSTANT-1";

const RULE_IDS: &[&str] = &[RULE_SHARED_CONSTANT];

/// The source files the literal extractor reads, plus the translation catalogues the i18n
/// guard (V1) scans for keys. The Layer-2 runner only loads files matching the armed checkers'
/// interest globs, so the catalogue directories must be declared here too.
const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.mjs",
    "**/*.cjs",
    "**/*.py",
    "**/*.rs",
    "**/locales/**/*.json",
    "**/locales/**/*.yaml",
    "**/locales/**/*.yml",
    "**/i18n/**/*.json",
    "**/i18n/**/*.yaml",
    "**/i18n/**/*.yml",
    "**/lang/**/*.json",
    "**/lang/**/*.yaml",
    "**/lang/**/*.yml",
    "**/langs/**/*.json",
    "**/langs/**/*.yaml",
    "**/langs/**/*.yml",
    "**/translations/**/*.json",
    "**/translations/**/*.yaml",
    "**/translations/**/*.yml",
    "**/messages/**/*.json",
    "**/messages/**/*.yaml",
    "**/messages/**/*.yml",
    "**/intl/**/*.json",
    "**/intl/**/*.yaml",
    "**/intl/**/*.yml",
];

pub struct SharedConstantChecker;

impl ArchChecker for SharedConstantChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let emitted = extract::paths::emitted_js_paths(repo.files);
        let mut index: BTreeMap<Key, Group<'_>> = BTreeMap::new();
        for (path, content) in repo.files {
            let Some(lang) = extract::lang_for_path(path) else {
                continue;
            };
            if is_excluded_file(path, content, &emitted) {
                continue;
            }
            let family = family_of(lang);
            for literal in extract::literals(lang, content) {
                let Some(shape) = shape_of(literal.kind, &literal.value) else {
                    continue;
                };
                // A protocol or style token is protocol or style text wherever it appears, so
                // dropping each occurrence on sight removes the value everywhere.
                if literal.kind == LiteralKind::Str && is_protocol_or_style_value(&literal.value) {
                    continue;
                }
                index
                    .entry((family, literal.kind, literal.value))
                    .or_insert_with(|| Group {
                        shape,
                        occurrences: Vec::new(),
                    })
                    .occurrences
                    .push(Occurrence {
                        file: path,
                        line: literal.line,
                        ctx: literal.ctx,
                    });
            }
        }
        let locale_keys = collect_locale_keys(repo.files);
        let mut out: Vec<ArchViolation> = index
            .into_iter()
            .filter_map(|((_, kind, value), group)| judge(&value, kind, &group, &locale_keys))
            .collect();
        out.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.message.as_str()).cmp(&(
                b.file.as_str(),
                b.line,
                b.message.as_str(),
            ))
        });
        out
    }
}

// ─── the index ──────────────────────────────────────────────────────────────────────────

/// A value is only ever compared within one language family: a TypeScript client and a Python
/// server writing the same route share a contract, not a constant, and "export one constant" is
/// not a fix across languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    Ecma,
    Python,
    Rust,
}

fn family_of(lang: SourceLang) -> Family {
    match lang {
        SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => Family::Ecma,
        SourceLang::Python => Family::Python,
        SourceLang::Rust => Family::Rust,
    }
}

type Key = (Family, LiteralKind, String);

/// One place a value is written.
struct Occurrence<'a> {
    file: &'a str,
    line: usize,
    ctx: LiteralCtx,
}

struct Group<'a> {
    shape: Shape,
    occurrences: Vec<Occurrence<'a>>,
}

// ─── the guards that read an occurrence's context (stubs until the safe twins drive them) ─

/// Value-level disqualifiers V1 to V11: one occurrence anywhere in the family that shows the
/// value is something other than a domain constant removes the value entirely.
fn context_disqualifies(shape: Shape, value: &str, ctx: &LiteralCtx) -> bool {
    let _ = (shape, value, ctx);
    false
}

/// Occurrence-level exclusions O1 to O4: this occurrence is not counted; the value survives.
fn occurrence_excluded(shape: Shape, value: &str, ctx: &LiteralCtx) -> bool {
    let _ = (shape, value, ctx);
    false
}

/// Every key found in a translation catalogue (V1's second half).
fn collect_locale_keys(files: &[(String, String)]) -> HashSet<String> {
    let _ = files;
    HashSet::new()
}

// ─── the verdict ────────────────────────────────────────────────────────────────────────

/// A module-level constant that holds the value.
struct Home<'a> {
    file: &'a str,
    line: usize,
    name: &'a str,
}

fn judge(
    value: &str,
    kind: LiteralKind,
    group: &Group<'_>,
    locale_keys: &HashSet<String>,
) -> Option<ArchViolation> {
    if group
        .occurrences
        .iter()
        .any(|o| context_disqualifies(group.shape, value, &o.ctx))
    {
        return None;
    }
    if kind == LiteralKind::Str && locale_keys.contains(value) {
        return None;
    }
    let counted: Vec<&Occurrence<'_>> = group
        .occurrences
        .iter()
        .filter(|o| !occurrence_excluded(group.shape, value, &o.ctx))
        .collect();

    // Homes: the initializer of a module-level constant, deduplicated by (file, name) so a
    // constant that lists the value twice is still one home.
    let mut homes: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for o in &counted {
        if let Some(home) = &o.ctx.module_const {
            let line = homes.entry((o.file, home.name.as_str())).or_insert(o.line);
            *line = (*line).min(o.line);
        }
    }
    let mut homes: Vec<Home<'_>> = homes
        .into_iter()
        .map(|((file, name), line)| Home { file, line, name })
        .collect();
    homes.sort_by_key(|h| (h.file, h.line, h.name));
    let home_files: BTreeSet<&str> = homes.iter().map(|h| h.file).collect();

    // Bare copies: everything else, minus the files that hold a home (a module using its own
    // constant's value next to it is not a cross-module copy).
    let bare: Vec<&Occurrence<'_>> = counted
        .iter()
        .copied()
        .filter(|o| o.ctx.module_const.is_none() && !home_files.contains(o.file))
        .collect();
    // A decimal rate is only a rate where it is arithmetic: one bare copy that is not an
    // operand means the number is something else here, and so everywhere.
    if kind == LiteralKind::Number && bare.iter().any(|o| !o.ctx.arithmetic) {
        return None;
    }
    let mut bare_files: BTreeMap<&str, usize> = BTreeMap::new();
    for o in &bare {
        let line = bare_files.entry(o.file).or_insert(o.line);
        *line = (*line).min(o.line);
    }
    let needed = if homes.is_empty() { 2 } else { 1 };
    if bare_files.len() < needed {
        return None;
    }
    let (first_file, first_line) = bare_files
        .iter()
        .map(|(f, l)| (*f, *l))
        .min_by_key(|(f, l)| (*f, *l))?;
    Some(ArchViolation {
        rule_id: RULE_SHARED_CONSTANT.to_string(),
        file: first_file.to_string(),
        line: first_line,
        object: Some(value.to_string()),
        message: message_for(value, &homes, &bare_files),
        // The corpus rule's `max_severity = "low"` is expected to clamp this at report time;
        // the checker contract forbids emitting `low` directly.
        severity: SEVERITY_MEDIUM,
    })
}

/// The finding text, in the client's voice: what is true, then what to do about it.
fn message_for(value: &str, homes: &[Home<'_>], bare_files: &BTreeMap<&str, usize>) -> String {
    let locations: Vec<String> = bare_files
        .iter()
        .map(|(file, line)| format!("`{file}:{line}`"))
        .collect();
    let list = join_with_and(&locations);
    let count = bare_files.len();
    match homes {
        [] => format!(
            "`{value}` is written as a bare literal in {count} files: {list}. Give it one home: \
             define it once as a named, exported constant and import that constant at every use, \
             so that changing the value is one edit and a search for the constant's name finds \
             every user."
        ),
        [home] => format!(
            "`{value}` is defined once as the constant `{}` in `{}:{}`, but is written as a bare \
             literal in {count} other file{}: {list}. Import the constant there instead of \
             repeating the value.",
            home.name,
            home.file,
            home.line,
            if count == 1 { "" } else { "s" },
        ),
        many => {
            let defs: Vec<String> = many
                .iter()
                .map(|h| format!("`{}` in `{}:{}`", h.name, h.file, h.line))
                .collect();
            format!(
                "`{value}` is defined as the constants {}, but is also written as a bare literal \
                 in {count} other file{}: {list}. Keep one of those constants as the home and \
                 import it at every use, including where the other definition sits, instead of \
                 repeating the value.",
                join_with_and(&defs),
                if count == 1 { "" } else { "s" },
            )
        }
    }
}

/// `a`, `a and b`, `a, b and c`.
fn join_with_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

// ─── the four value shapes ──────────────────────────────────────────────────────────────

/// Which of the four domain-literal shapes a value has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    /// S1: separated lowercase token.
    Token,
    /// S2: route template.
    Route,
    /// S3: multi-word SCREAMING_SNAKE.
    ScreamingSnake,
    /// N1: decimal rate.
    Rate,
}

/// S1: 4 to 64 characters of `[a-z0-9]` and the separators `- : /` plus `.` inside a segment,
/// split on the separators into at least two non-empty segments; at least one letter overall;
/// and no all-digit segment when a `:` is present (`host:port`).
fn is_separated_token(v: &str) -> bool {
    if !(4..=64).contains(&v.len()) {
        return false;
    }
    if !v.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b':' | b'/' | b'.')
    }) {
        return false;
    }
    let segments: Vec<&str> = v.split(['-', ':', '/']).collect();
    if segments.len() < 2
        || segments
            .iter()
            .any(|s| s.is_empty() || s.starts_with('.') || s.ends_with('.'))
    {
        return false;
    }
    if !v.bytes().any(|b| b.is_ascii_lowercase()) {
        return false;
    }
    !(v.contains(':')
        && segments
            .iter()
            .any(|s| s.bytes().all(|b| b.is_ascii_digit())))
}

/// S2: `/seg(/seg)+` where the first segment is a lowercase identifier and every later one is
/// an identifier, a `:param`, a `{param}`, a `[param]` or `*`.
fn is_route_template(v: &str) -> bool {
    let Some(rest) = v.strip_prefix('/') else {
        return false;
    };
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() < 2 || !is_route_identifier(segments[0]) {
        return false;
    }
    segments[1..].iter().all(|s| {
        is_route_identifier(s)
            || s.strip_prefix(':').is_some_and(is_param_name)
            || s.strip_prefix('{')
                .and_then(|r| r.strip_suffix('}'))
                .is_some_and(is_braced_param)
            || s.strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .is_some_and(is_braced_param)
            || *s == "*"
    })
}

/// `[a-z][a-z0-9_-]*`.
fn is_route_identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// `[a-z_][a-z0-9_]*`, the name of a `:param`.
fn is_param_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `[a-z_]+`, the name inside `{param}` / `[param]`.
fn is_braced_param(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// S3: `^[A-Z][A-Z0-9]*(_[A-Z0-9]+)+$`, 5 to 64 characters.
fn is_screaming_snake(v: &str) -> bool {
    if !(5..=64).contains(&v.len()) {
        return false;
    }
    let words: Vec<&str> = v.split('_').collect();
    words.len() >= 2
        && words.iter().all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
        && words[0].as_bytes()[0].is_ascii_uppercase()
}

/// N1: a canonical number strictly between 0 and 1 that is not one of the round fractions
/// (`0.5`, `0.25`, `0.75`), which are thresholds and midpoints far more often than rates.
fn is_decimal_rate(canonical: &str) -> bool {
    canonical.parse::<f64>().is_ok_and(|n| {
        n > 0.0 && n < 1.0 && ![0.5, 0.25, 0.75].contains(&n) && canonical.contains('.')
    })
}

/// The shape of a literal, or `None` when it has none of the four (the overwhelming case).
fn shape_of(kind: LiteralKind, value: &str) -> Option<Shape> {
    match kind {
        LiteralKind::Number => is_decimal_rate(value).then_some(Shape::Rate),
        LiteralKind::Str => {
            if is_separated_token(value) {
                Some(Shape::Token)
            } else if is_route_template(value) {
                Some(Shape::Route)
            } else if is_screaming_snake(value) {
                Some(Shape::ScreamingSnake)
            } else {
                None
            }
        }
    }
}

// ─── values that are protocol, styling or platform text, whatever their position ────────

/// Tailwind variants and utilities (V2): a value that starts with one is a class name.
const TAILWIND_PREFIXES: &[&str] = &[
    "sm:",
    "md:",
    "lg:",
    "xl:",
    "2xl:",
    "hover:",
    "focus:",
    "active:",
    "disabled:",
    "dark:",
    "group-hover:",
    "peer-",
    "first:",
    "last:",
    "odd:",
    "even:",
    "before:",
    "after:",
    "placeholder:",
    "print:",
    "text-",
    "bg-",
    "border-",
    "p-",
    "px-",
    "py-",
    "pt-",
    "pb-",
    "pl-",
    "pr-",
    "m-",
    "mx-",
    "my-",
    "mt-",
    "mb-",
    "ml-",
    "mr-",
    "w-",
    "h-",
    "min-w-",
    "min-h-",
    "max-w-",
    "max-h-",
    "gap-",
    "space-x-",
    "space-y-",
    "rounded-",
    "font-",
    "items-",
    "justify-",
    "self-",
    "place-",
    "grid-",
    "col-",
    "row-",
    "flex-",
    "shadow-",
    "ring-",
    "opacity-",
    "z-",
    "top-",
    "right-",
    "bottom-",
    "left-",
    "inset-",
    "overflow-",
    "leading-",
    "tracking-",
    "transition-",
    "duration-",
    "ease-",
    "delay-",
    "animate-",
    "scale-",
    "rotate-",
    "translate-",
    "cursor-",
    "select-",
    "outline-",
    "divide-",
    "fill-",
    "stroke-",
    "object-",
    "aspect-",
    "basis-",
    "grow-",
    "shrink-",
    "order-",
    "whitespace-",
    "break-",
    "line-clamp-",
    "list-",
    "decoration-",
    "align-",
    "content-",
    "backdrop-",
    "blur-",
    "accent-",
    "caret-",
    "resize-",
    "scroll-",
    "snap-",
    "touch-",
    "will-change-",
];

/// CSS keyword values that have the S1 shape (V2).
const CSS_KEYWORDS: &[&str] = &[
    "flex-start",
    "flex-end",
    "space-between",
    "space-around",
    "space-evenly",
    "inline-block",
    "inline-flex",
    "border-box",
    "content-box",
    "sans-serif",
    "pre-wrap",
    "break-word",
    "ease-in",
    "ease-out",
    "ease-in-out",
    "row-reverse",
    "column-reverse",
    "no-repeat",
];

/// The standard CSS property names, kebab-case (V2 and O4). A finite list: an unlisted
/// property whose value repeats across files would be reported (the residual risk the design
/// doc states), bounded by the shape rules and the position guards.
const CSS_PROPERTIES: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "all",
    "animation",
    "animation-delay",
    "animation-direction",
    "animation-duration",
    "animation-fill-mode",
    "animation-iteration-count",
    "animation-name",
    "animation-play-state",
    "animation-timing-function",
    "appearance",
    "aspect-ratio",
    "backdrop-filter",
    "backface-visibility",
    "background",
    "background-attachment",
    "background-blend-mode",
    "background-clip",
    "background-color",
    "background-image",
    "background-origin",
    "background-position",
    "background-repeat",
    "background-size",
    "border",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-color",
    "border-image",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-radius",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-spacing",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-top-width",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "caption-side",
    "caret-color",
    "clear",
    "clip",
    "clip-path",
    "color",
    "column-count",
    "column-gap",
    "column-rule",
    "column-width",
    "columns",
    "content",
    "counter-increment",
    "counter-reset",
    "cursor",
    "direction",
    "display",
    "empty-cells",
    "fill",
    "filter",
    "flex",
    "flex-basis",
    "flex-direction",
    "flex-flow",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "font",
    "font-family",
    "font-feature-settings",
    "font-kerning",
    "font-size",
    "font-size-adjust",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "gap",
    "grid",
    "grid-area",
    "grid-auto-columns",
    "grid-auto-flow",
    "grid-auto-rows",
    "grid-column",
    "grid-column-end",
    "grid-column-start",
    "grid-gap",
    "grid-row",
    "grid-row-end",
    "grid-row-start",
    "grid-template",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "hyphens",
    "image-rendering",
    "inset",
    "isolation",
    "justify-content",
    "justify-items",
    "justify-self",
    "left",
    "letter-spacing",
    "line-height",
    "list-style",
    "list-style-image",
    "list-style-position",
    "list-style-type",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "mask",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "mix-blend-mode",
    "object-fit",
    "object-position",
    "opacity",
    "order",
    "orphans",
    "outline",
    "outline-color",
    "outline-offset",
    "outline-style",
    "outline-width",
    "overflow",
    "overflow-wrap",
    "overflow-x",
    "overflow-y",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "page-break-after",
    "page-break-before",
    "page-break-inside",
    "perspective",
    "perspective-origin",
    "place-content",
    "place-items",
    "place-self",
    "pointer-events",
    "position",
    "quotes",
    "resize",
    "right",
    "rotate",
    "row-gap",
    "scale",
    "scroll-behavior",
    "scroll-margin",
    "scroll-padding",
    "scroll-snap-align",
    "scroll-snap-type",
    "shape-outside",
    "tab-size",
    "table-layout",
    "text-align",
    "text-align-last",
    "text-decoration",
    "text-decoration-color",
    "text-decoration-line",
    "text-decoration-style",
    "text-indent",
    "text-justify",
    "text-overflow",
    "text-shadow",
    "text-transform",
    "top",
    "touch-action",
    "transform",
    "transform-origin",
    "transform-style",
    "transition",
    "transition-delay",
    "transition-duration",
    "transition-property",
    "transition-timing-function",
    "translate",
    "unicode-bidi",
    "user-select",
    "vertical-align",
    "visibility",
    "white-space",
    "widows",
    "width",
    "will-change",
    "word-break",
    "word-spacing",
    "word-wrap",
    "writing-mode",
    "z-index",
    "zoom",
];

/// MIME top-level types (V6): `application/json`, `text/html`.
const MIME_PREFIXES: &[&str] = &[
    "application/",
    "text/",
    "image/",
    "audio/",
    "video/",
    "multipart/",
    "font/",
    "message/",
];

/// Character encodings, cache and fetch tokens, and auth schemes (V6).
const HTTP_TOKENS: &[&str] = &[
    "utf-8",
    "utf8",
    "base64",
    "base64url",
    "hex",
    "ascii",
    "latin1",
    "binary",
    "ucs-2",
    "utf-16le",
    "no-cache",
    "no-store",
    "must-revalidate",
    "same-origin",
    "same-site",
    "cross-origin",
    "strict-origin",
    "strict-origin-when-cross-origin",
    "no-referrer",
    "unsafe-url",
    "include",
    "omit",
    "cors",
    "no-cors",
    "navigate",
    "force-cache",
    "only-if-cached",
    "bearer",
    "basic",
    "digest",
];

/// Header-name prefixes (V6).
const HEADER_PREFIXES: &[&str] = &[
    "x-",
    "sec-",
    "if-",
    "accept-",
    "content-",
    "access-control-",
    "cache-",
    "proxy-",
    "www-",
];

/// Standard HTTP header names (V6), lowercase.
const HEADER_NAMES: &[&str] = &[
    "accept",
    "accept-charset",
    "accept-encoding",
    "accept-language",
    "accept-ranges",
    "age",
    "allow",
    "authorization",
    "cache-control",
    "connection",
    "content-disposition",
    "content-encoding",
    "content-language",
    "content-length",
    "content-location",
    "content-range",
    "content-security-policy",
    "content-type",
    "cookie",
    "cross-origin-embedder-policy",
    "cross-origin-opener-policy",
    "cross-origin-resource-policy",
    "date",
    "dnt",
    "etag",
    "expect",
    "expires",
    "forwarded",
    "host",
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-range",
    "if-unmodified-since",
    "keep-alive",
    "last-event-id",
    "last-modified",
    "link",
    "location",
    "max-forwards",
    "origin",
    "origin-agent-cluster",
    "permissions-policy",
    "pragma",
    "proxy-authenticate",
    "proxy-authorization",
    "range",
    "referer",
    "referrer-policy",
    "refresh",
    "retry-after",
    "server",
    "service-worker-allowed",
    "set-cookie",
    "strict-transport-security",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "upgrade-insecure-requests",
    "user-agent",
    "vary",
    "via",
    "warning",
    "www-authenticate",
];

/// Platform and target-triple tokens (V7).
const PLATFORM_TOKENS: &[&str] = &[
    "linux-x64",
    "darwin-arm64",
    "win32-x64",
    "x86_64",
    "aarch64",
    "wasm32-unknown-unknown",
];

/// Platform and HTML-attribute prefixes (V7).
const PLATFORM_PREFIXES: &[&str] = &[
    "linux-", "darwin-", "win32-", "freebsd-", "aria-", "data-", "xmlns-", "xlink-",
];

/// Extensions that mark a value as a file name (V8): the scan's code extensions plus common
/// asset and document extensions.
const FILE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "rb", "php", "cs", "sql",
    "toml", "yaml", "yml", "json", "sh", "env", "cfg", "ini", "tf", "kt", "swift", "c", "cpp", "h",
    "hcl", "bicep", "html", "css", "scss", "svg", "png", "jpg", "jpeg", "gif", "webp", "ico",
    "pdf", "csv", "md", "txt", "xml", "lock", "map", "woff", "woff2", "ttf",
];

/// Filesystem roots (V8): a value that starts with one is a path, not a route.
const PATH_PREFIXES: &[&str] = &[
    "./", "../", "~/", "/tmp/", "/usr/", "/etc/", "/var/", "/opt/", "/home/", "/dev/", "/proc/",
    "/mnt/", "/srv/",
];

/// Whether the value itself, with no context, is a protocol, styling, platform or filesystem
/// token (V2's Tailwind and CSS part, V6, V7, V8). Such a value is removed everywhere. Only
/// ever asked of STRING values: a number is never one of these tokens.
fn is_protocol_or_style_value(v: &str) -> bool {
    is_style_value(v) || is_http_protocol_value(v) || is_platform_value(v) || is_path_like_value(v)
}

fn is_style_value(v: &str) -> bool {
    TAILWIND_PREFIXES.iter().any(|p| v.starts_with(p))
        || CSS_KEYWORDS.contains(&v)
        || is_css_property(v)
}

/// Whether `name` is a CSS property, written camelCase or kebab-case (`justifyContent`,
/// `justify-content`). Used for the values of a style object (O4).
fn is_css_property(name: &str) -> bool {
    let mut kebab = String::with_capacity(name.len() + 2);
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            kebab.push('-');
            kebab.push(c.to_ascii_lowercase());
        } else {
            kebab.push(c);
        }
    }
    CSS_PROPERTIES.contains(&kebab.as_str())
}

fn is_http_protocol_value(v: &str) -> bool {
    MIME_PREFIXES.iter().any(|p| v.starts_with(p))
        || HTTP_TOKENS.contains(&v)
        || is_locale_tag(v)
        || is_semver(v)
        || is_uuid(v)
        || HEADER_PREFIXES.iter().any(|p| v.starts_with(p))
        || HEADER_NAMES.contains(&v)
}

/// `^[a-z]{2}(-[a-z]{2,4})?$`.
fn is_locale_tag(v: &str) -> bool {
    let all_lower = |s: &str| s.bytes().all(|b| b.is_ascii_lowercase());
    match v.split_once('-') {
        None => v.len() == 2 && all_lower(v),
        Some((lang, region)) => {
            lang.len() == 2
                && all_lower(lang)
                && (2..=4).contains(&region.len())
                && all_lower(region)
        }
    }
}

/// `^v?\d+(\.\d+)*(-[a-z0-9.]+)?$`.
fn is_semver(v: &str) -> bool {
    let v = v.strip_prefix('v').unwrap_or(v);
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (v, None),
    };
    let numeric = !core.is_empty()
        && core
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    numeric
        && pre.is_none_or(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.')
        })
}

/// `8-4-4-4-12` hexadecimal groups.
fn is_uuid(v: &str) -> bool {
    let groups: Vec<&str> = v.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_platform_value(v: &str) -> bool {
    PLATFORM_TOKENS.contains(&v) || PLATFORM_PREFIXES.iter().any(|p| v.starts_with(p))
}

/// V8: a file name, a filesystem path, a URL, or text with characters a name never carries.
fn is_path_like_value(v: &str) -> bool {
    if PATH_PREFIXES.iter().any(|p| v.starts_with(p)) || v.contains("://") {
        return true;
    }
    if v.rsplit_once('.')
        .is_some_and(|(_, ext)| FILE_EXTENSIONS.contains(&ext))
    {
        return true;
    }
    v.chars().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(c, '*' | '?' | '=' | '@' | '#' | '%' | '(' | '\\')
    })
}

// ─── files that are never a counted occurrence (F1 to F5) ───────────────────────────────

/// Directories whose files hold per-environment or deployment configuration (F2). Per-environment
/// config is SUPPOSED to repeat values across environments.
const CONFIG_DIRS: &[&str] = &[
    "settings",
    "config",
    "conf",
    "env",
    "environments",
    "deploy",
    "infra",
    "terraform",
    "k8s",
    "helm",
];

/// Migrations and generated code (F3).
const GENERATED_DIRS: &[&str] = &[
    "migrations",
    "migration",
    "alembic",
    "versions",
    "generated",
];

/// One-off code, docs and samples (F4): they repeat an application value on purpose, and "import
/// the constant" is often impossible from a standalone script.
const STANDALONE_DIRS: &[&str] = &[
    "scripts",
    "tools",
    "docs",
    "examples",
    "samples",
    "benchmarks",
    "seeds",
    "seed",
];

/// Basenames, up to the first dot, that a tool or framework loads by name (F2).
const TOOL_LOADED_STEMS: &[&str] = &[
    "setuptests",
    "test-setup",
    "instrumentation",
    "middleware",
    "proxy",
    "service-worker",
    "sw",
    "manifest",
    "reportwebvitals",
    "polyfills",
    "preload",
    "env",
    "global",
    "globals",
    "register",
    "vite-env",
    "next-env",
];

/// The first lines of a file that may carry a generated-code marker (F3).
const GENERATED_MARKER_LINES: usize = 10;

const GENERATED_MARKERS: &[&str] = &[
    "@generated",
    "Code generated",
    "DO NOT EDIT",
    "do not edit",
    "auto-generated",
    "automatically generated",
    "AUTOGENERATED",
];

/// Whether `path` (with its `content`) is excluded from counting altogether: tests, fixtures,
/// config, migrations, scripts, docs, generated code, declaration files and committed build
/// output. `emitted` is [`crate::extract::paths::emitted_js_paths`] over the whole repo.
fn is_excluded_file(path: &str, content: &str, emitted: &HashSet<&str>) -> bool {
    if emitted.contains(path) || extract::paths::is_test_path(path) {
        return true;
    }
    let lower = path.to_ascii_lowercase();
    let mut segments: Vec<&str> = lower.split('/').collect();
    let Some(base) = segments.pop() else {
        return true;
    };
    // F2: root-level files and anything inside a dot directory or named like a dotfile.
    if segments.is_empty() || segments.iter().any(|s| s.starts_with('.')) || base.starts_with('.') {
        return true;
    }
    if segments.iter().any(|s| {
        CONFIG_DIRS.contains(s) || GENERATED_DIRS.contains(s) || STANDALONE_DIRS.contains(s)
    }) {
        return true;
    }
    is_config_shaped_basename(base)
        || is_declaration_file(base)
        || content
            .lines()
            .take(GENERATED_MARKER_LINES)
            .any(|line| GENERATED_MARKERS.iter().any(|m| line.contains(m)))
}

/// F2 basenames: `*.config.*`, `*.conf.*`, `*.setup.*`, `*.workspace.*`, `*rc.{js,cjs,mjs,ts}`,
/// `settings.py`, `conftest.py` and the names tools load by convention.
fn is_config_shaped_basename(base: &str) -> bool {
    if base == "settings.py" || base == "conftest.py" {
        return true;
    }
    if [".config.", ".conf.", ".setup.", ".workspace."]
        .iter()
        .any(|m| base.contains(m))
    {
        return true;
    }
    if ["rc.js", "rc.cjs", "rc.mjs", "rc.ts"]
        .iter()
        .any(|suffix| base.ends_with(suffix))
    {
        return true;
    }
    let stem = base.split('.').next().unwrap_or(base);
    TOOL_LOADED_STEMS.contains(&stem)
}

/// F3: type declaration files, consumed by the compiler and never imported.
fn is_declaration_file(base: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts", ".pyi"]
        .iter()
        .any(|suffix| base.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
    }

    fn run(f: &[(String, String)]) -> Vec<ArchViolation> {
        SharedConstantChecker.check(&RepoView {
            spec: "test/repo",
            files: f,
        })
    }

    fn run_pairs(pairs: Vec<(&str, &str)>) -> Vec<ArchViolation> {
        run(&files(pairs))
    }

    // ── positives ──────────────────────────────────────────────────────────────

    #[test]
    fn kebab_queue_name_bare_in_two_files_is_flagged() {
        let v = run_pairs(vec![
            ("src/api/orders.ts", "publish('orders-high', msg);\n"),
            ("src/worker/consume.ts", "subscribe('orders-high', h);\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].rule_id, RULE_SHARED_CONSTANT);
        assert_eq!(v[0].object.as_deref(), Some("orders-high"));
        assert_eq!(v[0].file, "src/api/orders.ts");
        assert_eq!(v[0].line, 1);
        assert_eq!(v[0].severity, SEVERITY_MEDIUM);
        let m = &v[0].message;
        for needle in [
            "src/api/orders.ts:1",
            "src/worker/consume.ts:1",
            "bare literal in 2 files",
            "one home",
        ] {
            assert!(m.contains(needle), "{needle} missing: {m}");
        }
    }

    #[test]
    fn colon_channel_name_across_three_files_lists_all_locations() {
        let v = run_pairs(vec![
            ("src/a.ts", "emit('user:signup');\non('user:signup', h);\n"),
            ("src/b.ts", "\nemit('user:signup');\n"),
            ("src/c.ts", "\n\non('user:signup', h);\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        let m = &v[0].message;
        for needle in [
            "bare literal in 3 files",
            "src/a.ts:1",
            "src/b.ts:2",
            "src/c.ts:3",
        ] {
            assert!(m.contains(needle), "{needle} missing: {m}");
        }
    }

    #[test]
    fn route_template_bare_in_client_and_server_is_flagged() {
        let v = run_pairs(vec![
            ("src/client/api.ts", "fetch('/api/v1/orders');\n"),
            ("src/server/routes.ts", "router.get('/api/v1/orders', h);\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].object.as_deref(), Some("/api/v1/orders"));
    }

    #[test]
    fn route_in_python_decorator_is_counted() {
        let v = run_pairs(vec![
            (
                "app/orders.py",
                "@app.route('/api/v1/orders')\ndef orders():\n    pass\n",
            ),
            ("app/client.py", "client.get('/api/v1/orders')\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].object.as_deref(), Some("/api/v1/orders"));
    }

    #[test]
    fn route_in_rust_attribute_is_counted() {
        let v = run_pairs(vec![
            ("src/a.rs", "#[get(\"/api/v1/orders\")]\nasync fn a() {}\n"),
            ("src/b.rs", "fn b() { client.get(\"/api/v1/orders\"); }\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].object.as_deref(), Some("/api/v1/orders"));
    }

    #[test]
    fn screaming_snake_action_type_without_a_union_is_flagged() {
        let v = run_pairs(vec![
            (
                "src/actions.ts",
                "export function add() { return { type: 'ADD_TODO' }; }\n",
            ),
            (
                "src/reducer.ts",
                "switch (a.type) { case 'ADD_TODO': break; }\n",
            ),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].object.as_deref(), Some("ADD_TODO"));
    }

    #[test]
    fn decimal_rate_in_arithmetic_in_two_files_is_flagged() {
        let v = run_pairs(vec![
            ("src/cart.ts", "const t = total * 0.12;\n"),
            ("src/invoice.ts", "const t = subtotal * 0.12;\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].object.as_deref(), Some("0.12"));
    }

    #[test]
    fn number_spellings_normalize_to_one_value() {
        let v = run_pairs(vec![
            ("src/cart.ts", "const t = total * .12;\n"),
            ("src/invoice.ts", "const t = subtotal * 0.120;\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].object.as_deref(), Some("0.12"));
    }

    #[test]
    fn existing_home_with_one_bare_copy_elsewhere_is_flagged() {
        let v = run_pairs(vec![
            (
                "src/queues.ts",
                "export const ORDERS_QUEUE = 'orders-high';\n",
            ),
            ("src/worker.ts", "subscribe('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].file, "src/worker.ts");
        let m = &v[0].message;
        for needle in ["ORDERS_QUEUE", "src/queues.ts:1", "1 other file"] {
            assert!(m.contains(needle), "{needle} missing: {m}");
        }
    }

    #[test]
    fn as_const_object_member_is_a_home() {
        let v = run_pairs(vec![
            (
                "src/routes.ts",
                "export const Routes = { orders: '/api/orders' } as const;\n",
            ),
            ("src/a.ts", "fetch('/api/orders');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`Routes`"), "{}", v[0].message);
    }

    #[test]
    fn python_module_level_constant_is_a_home() {
        let v = run_pairs(vec![
            ("app/const.py", "ORDERS_QUEUE: Final = 'orders-high'\n"),
            ("app/worker.py", "q.subscribe('orders-high')\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`ORDERS_QUEUE`"), "{}", v[0].message);
    }

    #[test]
    fn rust_const_item_is_a_home() {
        let v = run_pairs(vec![
            ("src/cfg.rs", "pub const BUCKET: &str = \"user-uploads\";\n"),
            (
                "src/upload.rs",
                "fn up() { client.bucket(\"user-uploads\"); }\n",
            ),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`BUCKET`"), "{}", v[0].message);
    }

    #[test]
    fn findings_are_sorted_and_stable() {
        let input = vec![
            ("src/z.ts", "publish('orders-high');\nsend('a-queue');\n"),
            ("src/y.ts", "subscribe('orders-high');\n"),
            ("src/x.ts", "pull('a-queue');\n"),
        ];
        let first = run_pairs(input.clone());
        let mut reversed = input;
        reversed.reverse();
        let second = run_pairs(reversed);
        assert_eq!(first, second, "input order must not matter");
        assert_eq!(first.len(), 2, "{first:#?}");
        let keys: Vec<(&str, usize)> = first.iter().map(|v| (v.file.as_str(), v.line)).collect();
        assert_eq!(keys, [("src/x.ts", 1), ("src/y.ts", 1)]);
    }

    // ── shape classifier ───────────────────────────────────────────────────────

    #[test]
    fn s1_matches_separated_lowercase_tokens() {
        for v in [
            "orders-high",
            "user:signup",
            "uploads/avatars",
            "feature-new-checkout",
            "svc-orders.v2",
            "order:created",
            "a1-b2",
            "cache:orders:v2",
        ] {
            assert!(is_separated_token(v), "{v}");
            assert_eq!(shape_of(LiteralKind::Str, v), Some(Shape::Token), "{v}");
        }
    }

    #[test]
    fn s1_rejects_everything_the_spec_names_and_the_boundaries() {
        for v in [
            "order_created",     // snake only
            "orders.created",    // dot only
            "Content-Type",      // uppercase
            "localhost:3000",    // host:port
            "db:5432",           // host:port
            "2024-01-01",        // no letter
            "127.0.0.1",         // no separator
            "0.0.0.0",           // no separator
            "-orders-high",      // starts with a separator
            "orders-high-",      // ends with a separator
            "orders--high",      // empty segment
            "a-b",               // too short
            "abc",               // too short, single word
            "orders",            // single word
            "pending",           // single word
            "orders-.high",      // segment starts with a dot
            "orders-high.",      // segment ends with a dot
            "orders high",       // whitespace
            "orders_high-low",   // underscore
            "Orders-High",       // uppercase
            "/api/v1/orders",    // a route, S2 not S1
            "https://x.example", // URL
        ] {
            assert!(!is_separated_token(v), "{v}");
        }
        let long = format!("a-{}", "b".repeat(63));
        assert_eq!(long.len(), 65);
        assert!(!is_separated_token(&long), "65 chars is over the limit");
        let max = format!("a-{}", "b".repeat(62));
        assert_eq!(max.len(), 64);
        assert!(is_separated_token(&max), "64 chars is allowed");
    }

    #[test]
    fn s2_matches_route_templates_with_at_least_two_segments() {
        for v in [
            "/api/v1/orders",
            "/orders/:id",
            "/users/{id}/avatar",
            "/users/[id]/avatar",
            "/api/orders/*",
            "/api/users/:user_id/posts",
            "/health/live",
        ] {
            assert!(is_route_template(v), "{v}");
            assert_eq!(shape_of(LiteralKind::Str, v), Some(Shape::Route), "{v}");
        }
    }

    #[test]
    fn s2_rejects_roots_single_segments_files_and_malformed_templates() {
        for v in [
            "/",
            "/api",
            "/tmp/x.txt",
            "api/v1/orders",   // not rooted
            "/api//orders",    // empty segment
            "/api/orders/",    // trailing slash
            "/Api/orders",     // uppercase
            "/api/1orders",    // segment must start with a letter
            "/api/:1id",       // param must start with a letter or underscore
            "/api/{Id}",       // uppercase param
            "/api/{}",         // empty param
            "/api/orders?x=1", // query string
            "/api/or ders",
        ] {
            assert!(!is_route_template(v), "{v}");
        }
    }

    #[test]
    fn s3_matches_multi_word_screaming_snake() {
        for v in [
            "ORDER_CREATED",
            "IN_PROGRESS",
            "ADD_TODO",
            "HTTP_2_UPGRADE",
            "A1_B2C",
        ] {
            assert!(is_screaming_snake(v), "{v}");
            assert_eq!(
                shape_of(LiteralKind::Str, v),
                Some(Shape::ScreamingSnake),
                "{v}"
            );
        }
    }

    #[test]
    fn s3_rejects_single_words_malformed_names_and_the_boundaries() {
        for v in [
            "PENDING",
            "GET",
            "UTF8",
            "A_B",      // under 5 characters
            "_ORDER_X", // leading underscore
            "ORDER_",   // trailing underscore
            "ORDER__X", // empty word
            "1ORDER_X", // starts with a digit
            "Order_Created",
            "order_created",
        ] {
            assert!(!is_screaming_snake(v), "{v}");
        }
        let long = format!("A_{}", "B".repeat(63));
        assert_eq!(long.len(), 65);
        assert!(!is_screaming_snake(&long));
        let max = format!("A_{}", "B".repeat(62));
        assert!(is_screaming_snake(&max));
    }

    #[test]
    fn n1_matches_decimals_strictly_between_zero_and_one_except_the_round_fractions() {
        for v in ["0.12", "0.15", "0.01", "0.999", "0.2", "0.1"] {
            assert!(is_decimal_rate(v), "{v}");
            assert_eq!(shape_of(LiteralKind::Number, v), Some(Shape::Rate), "{v}");
        }
        for v in [
            "0.5", "0.25", "0.75", "0", "1", "1.5", "12", "86400", "60", "1000", "404", "100",
            "-0.12", "NaN", "abc", "",
        ] {
            assert!(!is_decimal_rate(v), "{v}");
        }
    }

    #[test]
    fn a_string_that_spells_a_number_is_not_a_rate_and_a_number_is_not_a_token() {
        assert_eq!(shape_of(LiteralKind::Str, "0.12"), None);
        assert_eq!(shape_of(LiteralKind::Number, "orders-high"), None);
    }

    // ── value-level protocol, style and platform tokens ────────────────────────

    #[test]
    fn tailwind_variants_and_utilities_are_style_values() {
        for v in [
            "text-gray-500",
            "bg-red-500",
            "items-center",
            "justify-between",
            "hover:underline",
            "sm:flex-row",
            "dark:bg-black",
            "md:px-4",
            "group-hover:visible",
            "peer-checked",
            "min-w-0",
            "line-clamp-3",
            "will-change-transform",
            "2xl:text-lg",
        ] {
            assert!(is_protocol_or_style_value(v), "{v}");
        }
    }

    #[test]
    fn css_keywords_and_property_names_are_style_values() {
        for v in [
            "flex-start",
            "flex-end",
            "space-between",
            "space-around",
            "space-evenly",
            "inline-block",
            "inline-flex",
            "border-box",
            "content-box",
            "sans-serif",
            "pre-wrap",
            "break-word",
            "ease-in",
            "ease-out",
            "ease-in-out",
            "row-reverse",
            "column-reverse",
            "no-repeat",
            "margin-top",
            "background-color",
            "text-align",
            "z-index",
        ] {
            assert!(is_protocol_or_style_value(v), "{v}");
        }
    }

    #[test]
    fn css_property_names_match_in_camel_and_kebab_case() {
        for v in [
            "justifyContent",
            "justify-content",
            "backgroundColor",
            "marginTop",
            "zIndex",
            "opacity",
            "transition",
            "gridTemplateColumns",
        ] {
            assert!(is_css_property(v), "{v}");
        }
        for v in ["orders", "queue", "name", "env", "type", "ordersHigh", ""] {
            assert!(!is_css_property(v), "{v}");
        }
    }

    #[test]
    fn http_protocol_tokens_are_excluded() {
        for v in [
            "application/json",
            "text/html",
            "image/png",
            "multipart/form-data",
            "font/woff2",
            "no-cache",
            "no-store",
            "must-revalidate",
            "same-origin",
            "same-site",
            "cross-origin",
            "strict-origin-when-cross-origin",
            "no-referrer",
            "unsafe-url",
            "force-cache",
            "only-if-cached",
            "no-cors",
            "utf-8",
            "base64url",
            "utf-16le",
            "en-us",
            "pt-br",
            "zh-hans",
            "1.2.0-beta",
            "v2.0.1",
            "123e4567-e89b-12d3-a456-426614174000",
            "x-request-id",
            "sec-fetch-mode",
            "if-none-match",
            "accept-language",
            "content-type",
            "access-control-allow-origin",
            "cache-control",
            "proxy-authorization",
            "www-authenticate",
            "user-agent",
            "set-cookie",
            "last-modified",
            "retry-after",
            "transfer-encoding",
            "keep-alive",
        ] {
            assert!(is_protocol_or_style_value(v), "{v}");
        }
    }

    #[test]
    fn platform_and_html_attribute_tokens_are_excluded() {
        for v in [
            "linux-x64",
            "darwin-arm64",
            "win32-x64",
            "linux-musl",
            "freebsd-x64",
            "wasm32-unknown-unknown",
            "aria-label",
            "aria-hidden",
            "data-testid",
            "xmlns-xlink",
            "xlink-href",
        ] {
            assert!(is_protocol_or_style_value(v), "{v}");
        }
        assert!(is_protocol_or_style_value("x86_64"));
        assert!(is_protocol_or_style_value("aarch64"));
    }

    #[test]
    fn filesystem_paths_urls_and_odd_characters_are_excluded() {
        for v in [
            "./data/seed.json",
            "../shared/config",
            "~/orders/cache",
            "/tmp/cache",
            "/usr/local/bin",
            "/etc/orders/app",
            "/var/lib/orders",
            "/opt/orders/run",
            "/home/app/orders",
            "/dev/shm/orders",
            "/proc/self/exe",
            "/mnt/data/orders",
            "/srv/orders/www",
            "config/default.yaml",
            "assets/logo.svg",
            "orders/export.csv",
            "docs/readme.md",
            "src/main.ts",
            "https://api.example.com/v1",
            "orders-*",
            "orders-high?x",
            "a=b-c",
            "user@host-name",
            "orders#high",
            "100%-width",
            "orders-(high)",
            "orders high-low",
            "orders-high\n",
        ] {
            assert!(is_protocol_or_style_value(v), "{v}");
        }
    }

    #[test]
    fn domain_values_are_not_protocol_or_style_values() {
        for v in [
            "orders-high",
            "user:signup",
            "uploads/avatars",
            "feature-new-checkout",
            "svc-orders.v2",
            "order:created",
            "/api/v1/orders",
            "/orders/:id",
            "/users/{id}/avatar",
            "ORDER_CREATED",
            "ADD_TODO",
            "IN_PROGRESS",
            "user-uploads",
            "retry-queue",
            "tenant-events",
        ] {
            assert!(!is_protocol_or_style_value(v), "{v}");
        }
    }

    // ── file-level exclusions ──────────────────────────────────────────────────

    fn excluded(path: &str) -> bool {
        is_excluded_file(path, "", &HashSet::new())
    }

    #[test]
    fn f1_test_fixture_mock_story_and_seed_paths_are_excluded() {
        for p in [
            "src/a.test.ts",
            "src/a.spec.tsx",
            "src/tests/a.ts",
            "src/__tests__/a.ts",
            "tests/test_orders.py",
            "app/orders_test.py",
            "src/fixtures/x.ts",
            "fixtures/x.ts",
            "src/__mocks__/x.ts",
            "e2e/login.ts",
            "cypress/support/a.ts",
            "src/a.stories.tsx",
            "app/conftest.py",
            "conftest.py",
            "app/seeds/orders.py",
            "db/seed/orders.ts",
            "src/test/helper.rs",
        ] {
            assert!(excluded(p), "{p}");
        }
        for p in [
            "src/latest/a.ts",
            "src/contest.ts",
            "app/orders.py",
            "src/seeded.ts",
            "src/attest.rs",
        ] {
            assert!(!excluded(p), "{p}");
        }
    }

    #[test]
    fn f2_root_level_dot_dir_and_config_shaped_files_are_excluded() {
        for p in [
            "main.py",
            "index.ts",
            "lib.rs",
            ".storybook/preview.ts",
            ".github/scripts/a.py",
            "src/.eslintrc.js",
            "src/vite.config.ts",
            "vitest.config.ts",
            "src/jest.setup.ts",
            "src/app.conf.js",
            "src/eslintrc.js",
            "src/setupTests.ts",
            "src/test-setup.ts",
            "src/app.workspace.ts",
            "src/instrumentation.ts",
            "src/middleware.ts",
            "src/proxy.ts",
            "src/service-worker.ts",
            "src/sw.js",
            "src/manifest.ts",
            "src/reportWebVitals.ts",
            "src/polyfills.ts",
            "src/preload.ts",
            "src/env.ts",
            "src/global.ts",
            "src/globals.ts",
            "src/register.ts",
            "src/vite-env.d.ts",
            "src/next-env.d.ts",
        ] {
            assert!(excluded(p), "{p}");
        }
    }

    #[test]
    fn f2_settings_and_deployment_directories_are_excluded_at_any_depth() {
        for p in [
            "app/settings.py",
            "app/settings/dev.py",
            "config/settings/prod.py",
            "src/config/queues.ts",
            "apps/api/conf/a.ts",
            "src/env/a.ts",
            "src/environments/environment.ts",
            "deploy/a.py",
            "infra/a.ts",
            "terraform/a.py",
            "k8s/a.py",
            "helm/a.py",
        ] {
            assert!(excluded(p), "{p}");
        }
        for p in [
            "src/api/orders.ts",
            "src/settings_view.ts",
            "src/configuration/a.ts",
            "src/envelope.ts",
            "app/orders/settings_page.py",
        ] {
            assert!(!excluded(p), "{p}");
        }
    }

    #[test]
    fn f3_migrations_generated_declaration_and_emitted_files_are_excluded() {
        for p in [
            "db/migrations/0001.py",
            "db/migration/a.ts",
            "alembic/env_a.py",
            "app/alembic/versions/a.py",
            "app/versions/a.py",
            "src/generated/a.ts",
            "src/types.d.ts",
            "src/types.d.mts",
            "src/types.d.cts",
            "app/orders.pyi",
        ] {
            assert!(excluded(p), "{p}");
        }
        let emitted: HashSet<&str> = ["src/a.js"].into_iter().collect();
        assert!(is_excluded_file("src/a.js", "", &emitted));
        assert!(!is_excluded_file("src/b.js", "", &emitted));
    }

    #[test]
    fn f3_generated_markers_in_the_first_ten_lines_exclude_a_file() {
        for marker in [
            "// @generated",
            "// Code generated by protoc. DO NOT EDIT.",
            "# DO NOT EDIT",
            "# do not edit",
            "// auto-generated by x",
            "// automatically generated",
            "// AUTOGENERATED",
        ] {
            let src = format!("{marker}\nconst a = 1;\n");
            assert!(
                is_excluded_file("src/a.ts", &src, &HashSet::new()),
                "{marker}"
            );
        }
        let line_ten = format!("{}// @generated\n", "\n".repeat(9));
        assert!(is_excluded_file("src/a.ts", &line_ten, &HashSet::new()));
        let line_eleven = format!("{}// @generated\n", "\n".repeat(10));
        assert!(!is_excluded_file("src/a.ts", &line_eleven, &HashSet::new()));
        assert!(!is_excluded_file(
            "src/a.ts",
            "const a = 1;\n",
            &HashSet::new()
        ));
    }

    #[test]
    fn f4_scripts_tools_docs_examples_samples_and_benchmarks_are_excluded() {
        for p in [
            "scripts/a.ts",
            "app/scripts/a.py",
            "tools/a.rs",
            "docs/snippets/a.ts",
            "examples/a.py",
            "samples/a.ts",
            "benchmarks/a.rs",
        ] {
            assert!(excluded(p), "{p}");
        }
        assert!(!excluded("src/descriptions/a.ts"));
        assert!(!excluded("src/scripting/a.ts"));
    }
}
