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

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

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
    // Package manifests: they mark where one package ends and the next begins.
    "**/package.json",
    "**/pyproject.toml",
    "**/Cargo.toml",
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
        let roots = PackageRoots::new(repo.files);
        let mut index: HashMap<Key, Group<'_>> = HashMap::new();
        for (path, content) in repo.files {
            let Some(lang) = extract::lang_for_path(path) else {
                continue;
            };
            let usage = classify_file(path, content, &emitted);
            if usage == FileUse::Skipped {
                continue;
            }
            let family = family_of(lang);
            let root = roots.of(path);
            for literal in extract::literals(lang, content) {
                // A class body the walker cannot name is neither a home nor a copy, and a
                // configuration file contributes the constants it defines but never a copy.
                if literal.ctx.unattributed
                    || (usage == FileUse::HomesOnly && literal.ctx.module_const.is_none())
                {
                    continue;
                }
                let Some(shape) = shape_of(literal.kind, &literal.value) else {
                    continue;
                };
                let kind = literal.kind;
                let group = match index.entry((family, root.clone(), kind, literal.value)) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        // A protocol or style token is protocol or style text wherever it
                        // appears, so it is decided once per distinct value and the value
                        // collects nothing.
                        let protocol =
                            kind == LiteralKind::Str && is_protocol_or_style_value(&e.key().3);
                        e.insert(Group {
                            shape,
                            protocol,
                            occurrences: Vec::new(),
                        })
                    }
                };
                if !group.protocol {
                    group.occurrences.push(Occurrence {
                        file: path,
                        line: literal.line,
                        ctx: literal.ctx,
                    });
                }
            }
        }
        let locale_keys = collect_locale_keys(repo.files);
        let mut out: Vec<ArchViolation> = index
            .into_iter()
            .filter(|(_, group)| !group.protocol)
            .filter_map(|((_, _, kind, value), group)| judge(&value, kind, &group, &locale_keys))
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

// ─── packages ───────────────────────────────────────────────────────────────────────────

/// Top-level directories that conventionally hold one package per child directory.
const PACKAGE_CONTAINERS: &[&str] = &[
    "apps", "packages", "services", "libs", "crates", "modules", "projects",
];

/// Where one package ends and the next begins. A value is compared only within one package: two
/// apps of a monorepo cannot share a constant unless a shared package exists, so a route or a
/// queue name they both write is two decisions, not one repeated literal. A package is the
/// nearest directory holding a `package.json`, `pyproject.toml` or `Cargo.toml` (below the
/// repository root), else the child directory of a conventional monorepo container
/// (`apps/web`), else the whole repository.
struct PackageRoots {
    manifest_dirs: HashSet<String>,
}

impl PackageRoots {
    fn new(files: &[(String, String)]) -> Self {
        let manifest_dirs = files
            .iter()
            .filter_map(|(path, _)| {
                let (dir, base) = path.rsplit_once('/')?;
                matches!(base, "package.json" | "pyproject.toml" | "Cargo.toml")
                    .then(|| dir.to_string())
            })
            .collect();
        Self { manifest_dirs }
    }

    fn of(&self, path: &str) -> String {
        let mut dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        while !dir.is_empty() {
            if self.manifest_dirs.contains(dir) {
                return dir.to_string();
            }
            dir = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        let mut segments = path.split('/');
        match (segments.next(), segments.next(), segments.next()) {
            (Some(container), Some(package), Some(_))
                if PACKAGE_CONTAINERS.contains(&container) =>
            {
                format!("{container}/{package}")
            }
            _ => String::new(),
        }
    }
}

// ─── the index ──────────────────────────────────────────────────────────────────────────

/// A value is only ever compared within one language family: a TypeScript client and a Python
/// server writing the same route share a contract, not a constant, and "export one constant" is
/// not a fix across languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// `(language family, package root, kind, value)`.
type Key = (Family, String, LiteralKind, String);

/// One place a value is written.
struct Occurrence<'a> {
    file: &'a str,
    line: usize,
    ctx: LiteralCtx,
}

struct Group<'a> {
    shape: Shape,
    /// The value is protocol, style, platform or path text (decided once, from the value alone).
    protocol: bool,
    occurrences: Vec<Occurrence<'a>>,
}

// ─── the guards that read an occurrence's context ────────────────────────────────────────

/// V1: call names that look a message up by key.
const I18N_CALLEES: &[&str] = &[
    "t",
    "$t",
    "i18n",
    "i18next",
    "translate",
    "tr",
    "gettext",
    "ngettext",
    "pgettext",
    "_",
    "__",
    "formatMessage",
    "defineMessage",
    "useTranslation",
];

/// V2: JSX attributes whose value is a class name or an element id.
const CLASS_ATTRS: &[&str] = &["className", "class", "classList", "id", "htmlFor", "for"];

/// V2: calls whose arguments are class names or style text.
const CLASS_CALLEES: &[&str] = &[
    "cn",
    "cx",
    "clsx",
    "classnames",
    "classNames",
    "cva",
    "tv",
    "twMerge",
    "twJoin",
    "tw",
    "css",
    "styled",
    "keyframes",
    "createGlobalStyle",
    "sx",
];

/// V3: calls that read an environment variable by name.
const ENV_CALLEES: &[&str] = &[
    "getenv",
    "env",
    "var",
    "var_os",
    "environ",
    "option_env",
    "set_var",
    "remove_var",
    "setdefault",
    "putenv",
    "unsetenv",
    // python-decouple's `config('NAME')`.
    "config",
];

/// V3: receivers that are the process environment or Django's request metadata.
const ENV_RECEIVER_SUFFIXES: &[&str] = &["env", "environ", "META"];

/// V6: request and response objects whose `get`/`set` verbs take a header name.
const HEADER_RECEIVERS: &[&str] = &["req", "res", "request", "response", "headers"];

/// Scope decision B: calls whose string argument is a Django URL name (`orders:detail`).
const URL_NAME_CALLEES: &[&str] = &["reverse", "reverse_lazy", "redirect", "resolve_url"];

/// V6: calls that take a header name directly.
const HEADER_CALLEES: &[&str] = &["setHeader", "getHeader", "header", "headers"];

/// V6: verbs that take a header name when the receiver is a headers object.
const HEADER_VERBS: &[&str] = &["get", "set", "has", "append", "delete", "insert"];

/// V9: calls whose argument names a logger or a tracing span.
const LOGGER_CALLEES: &[&str] = &[
    "getLogger",
    "get_logger",
    "child",
    "logger",
    "span",
    "instrument",
    "trace_span",
    "info_span",
    "debug_span",
    // OpenTelemetry and Sentry span and tag names.
    "start_as_current_span",
    "start_span",
    "startSpan",
    "startActiveSpan",
    "setTag",
    "set_tag",
    "setAttribute",
    "set_attribute",
];

/// V10: calls whose argument is a query fragment or a column path.
const SQL_CALLEES: &[&str] = &[
    "sql", "raw", "query", "execute", "prepare", "select", "insert", "update", "delete", "where",
    "orderBy", "groupBy",
];

/// V11: calls that subscribe to, or emit, an event.
const EVENT_CALLEES: &[&str] = &[
    "addEventListener",
    "removeEventListener",
    "on",
    "once",
    "off",
    "emit",
];

/// V11: DOM and runtime event names. Custom colon and kebab names (`order:created`) are the
/// rule's own subject and are not here.
const DOM_EVENTS: &[&str] = &[
    "click",
    "change",
    "input",
    "submit",
    "keydown",
    "keyup",
    "resize",
    "scroll",
    "load",
    "error",
    "exit",
    "SIGTERM",
    "SIGINT",
    "uncaughtException",
    "unhandledRejection",
    "message",
    "connection",
    "close",
    "data",
    "end",
];

/// Value-level disqualifiers V1 to V11: one occurrence anywhere in the family that shows the
/// value is something other than a domain constant removes the value entirely. Numbers have
/// none: the rate shape is guarded by its arithmetic requirement instead.
fn context_disqualifies(shape: Shape, value: &str, ctx: &LiteralCtx) -> bool {
    if shape == Shape::Rate {
        return false;
    }
    let callee = ctx.callee.as_deref().unwrap_or("");
    let chain = ctx.callee_chain.as_deref().unwrap_or("");
    let in_callees = |list: &[&str]| list.contains(&callee);
    // V1 i18n keys.
    if in_callees(I18N_CALLEES) {
        return true;
    }
    // V2 class names and style text.
    if ctx
        .jsx_attr
        .as_deref()
        .is_some_and(|a| CLASS_ATTRS.contains(&a))
        || in_callees(CLASS_CALLEES)
        || chain.contains("classList")
    {
        return true;
    }
    // V3 environment variable names (the SCREAMING_SNAKE shape): read by name through a call
    // (`getenv`, `config`, `set_var`), through a receiver that is the environment (any verb:
    // `Deno.env.get`, `os.environ.setdefault`, `request.META.get`), or by subscript.
    if shape == Shape::ScreamingSnake
        && (in_callees(ENV_CALLEES)
            || ends_with_any(call_receiver(chain), ENV_RECEIVER_SUFFIXES)
            || (ctx.subscript
                && ctx
                    .subscript_receiver
                    .as_deref()
                    .is_some_and(|r| ends_with_any(r, ENV_RECEIVER_SUFFIXES)))
            || ctx.value_of_key.as_deref() == Some("env"))
    {
        return true;
    }
    // V4 typed enum members (the token and SCREAMING_SNAKE shapes): the type IS the home.
    if shape != Shape::Route && ctx.type_position {
        return true;
    }
    // V5 import specifiers.
    if ctx.import_specifier {
        return true;
    }
    // V6 header names, by the call that takes them.
    if in_callees(HEADER_CALLEES)
        || (in_callees(HEADER_VERBS)
            && (chain.to_ascii_lowercase().contains("header")
                || last_segment(call_receiver(chain))
                    .is_some_and(|r| HEADER_RECEIVERS.contains(&r))))
    {
        return true;
    }
    // Scope decision B: a Django URL name. A route passed to `redirect('/login')` is a route.
    if shape != Shape::Route && in_callees(URL_NAME_CALLEES) {
        return true;
    }
    // V9 logger and span names, V10 query fragments.
    if in_callees(LOGGER_CALLEES) || in_callees(SQL_CALLEES) {
        return true;
    }
    // V11 DOM and runtime event names.
    in_callees(EVENT_CALLEES) && DOM_EVENTS.contains(&value)
}

/// The receiver part of a callee chain: `Deno.env` for `Deno.env.get`, `std::env` for
/// `std::env::set_var`, empty for a bare call.
fn call_receiver(chain: &str) -> &str {
    let dot = chain.rfind('.');
    let path = chain.rfind("::");
    match (dot, path) {
        (Some(d), Some(p)) if p > d => &chain[..p],
        (Some(d), _) => &chain[..d],
        (None, Some(p)) => &chain[..p],
        (None, None) => "",
    }
}

fn last_segment(receiver: &str) -> Option<&str> {
    receiver
        .rsplit(['.', ':'])
        .next()
        .filter(|segment| !segment.is_empty())
}

fn ends_with_any(text: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| text.ends_with(suffix))
}

/// Occurrence-level exclusions O1 to O4: this occurrence is not counted; the value survives.
fn occurrence_excluded(shape: Shape, _value: &str, ctx: &LiteralCtx) -> bool {
    // O1 a module specifier; O2 a wire-format or build-feature name in an attribute (a route in
    // an attribute IS a route definition and counts); O3 a key of the data's schema; O4 the
    // value of a CSS property.
    ctx.import_specifier
        || (ctx.attribute && shape != Shape::Route)
        // An HTML attribute value (`autoComplete`, `type`, `name`, `data-testid`) is markup,
        // except a route in `href` or `to`.
        || (ctx.jsx_attr.is_some() && shape != Shape::Route)
        // A member of a type is the type's, not a copy (a constant that IS the type, such as an
        // enum member, stays a home).
        || (ctx.type_position && ctx.module_const.is_none())
        || ctx.object_key
        || ctx.subscript
        || ctx.value_of_key.as_deref().is_some_and(is_css_property)
}

// ─── translation catalogues (V1's second half) ──────────────────────────────────────────

/// Directories that hold translation catalogues.
const CATALOGUE_DIRS: &[&str] = &[
    "locales",
    "i18n",
    "lang",
    "langs",
    "translations",
    "messages",
    "intl",
];

/// Every key found in a translation catalogue under one of [`CATALOGUE_DIRS`]: a cheap text
/// scan for `"key":` in JSON and `key:` in YAML. A value that is a catalogue key is a message
/// id, whichever call reads it.
fn collect_locale_keys(files: &[(String, String)]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for (path, content) in files {
        let lower = path.to_ascii_lowercase();
        let mut segments: Vec<&str> = lower.split('/').collect();
        let Some(base) = segments.pop() else {
            continue;
        };
        if !segments.iter().any(|s| CATALOGUE_DIRS.contains(s)) {
            continue;
        }
        if base.ends_with(".json") {
            json_keys(content, &mut keys);
        } else if base.ends_with(".yaml") || base.ends_with(".yml") {
            yaml_keys(content, &mut keys);
        }
    }
    keys
}

/// Strings in `text` that are followed (after optional whitespace) by a colon.
fn json_keys(text: &str, out: &mut HashSet<String>) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut key = String::new();
        let mut closed = false;
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        key.push(escaped);
                    }
                }
                '"' => {
                    closed = true;
                    break;
                }
                other => key.push(other),
            }
        }
        if !closed {
            return;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek() == Some(&':') {
            out.insert(key);
        }
    }
}

/// The key on each `key: value` line, quoted or bare, with list dashes ignored.
fn yaml_keys(text: &str, out: &mut HashSet<String>) {
    for line in text.lines() {
        let mut rest = line.trim_start();
        if rest.starts_with('#') {
            continue;
        }
        while let Some(after) = rest.strip_prefix("- ") {
            rest = after.trim_start();
        }
        let key = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..]
                .split_once(quote)
                .and_then(|(key, after)| after.trim_start().starts_with(':').then_some(key)),
            _ => rest
                .split_once(':')
                .filter(|(_, after)| after.is_empty() || after.starts_with(char::is_whitespace))
                .map(|(key, _)| key.trim_end()),
        };
        if let Some(key) = key.filter(|k| !k.is_empty()) {
            out.insert(key.to_string());
        }
    }
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

/// N1: a canonical number strictly between 0 and 1 with at least two significant digits that is
/// not one of the round fractions (`0.5`, `0.25`, `0.75`), which are thresholds and midpoints far
/// more often than rates.
fn is_decimal_rate(canonical: &str) -> bool {
    let in_range = canonical.parse::<f64>().is_ok_and(|n| {
        n > 0.0 && n < 1.0 && ![0.5, 0.25, 0.75].contains(&n) && canonical.contains('.')
    });
    // At least two significant digits (scope decision A): `0.12` and `0.075` are rates; `0.9`,
    // `0.001` and `1e-9` are scales, units and epsilons.
    in_range
        && canonical
            .split_once('.')
            .is_some_and(|(_, fraction)| fraction.trim_start_matches('0').len() >= 2)
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
    is_style_value(v)
        || is_http_protocol_value(v)
        || is_platform_value(v)
        || is_runtime_identifier(v)
        || is_path_like_value(v)
}

/// A lazily built hash set over one of the static name lists above, so a membership test is a
/// hash lookup rather than a scan (the lists are consulted once per distinct value).
fn name_set(
    cell: &'static OnceLock<HashSet<&'static str>>,
    list: &'static [&'static str],
) -> &'static HashSet<&'static str> {
    cell.get_or_init(|| list.iter().copied().collect())
}

fn css_property_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    name_set(&SET, CSS_PROPERTIES)
}

fn header_name_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    name_set(&SET, HEADER_NAMES)
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
    css_property_set().contains(kebab.as_str())
}

/// Cipher and digest algorithm names, which are standardised identifiers (Node `crypto`,
/// WebCrypto): `aes-256-gcm`, `sha-256`, `rsa-oaep`.
const CRYPTO_PREFIXES: &[&str] = &[
    "aes-",
    "des-",
    "rsa-",
    "sha-",
    "hmac-",
    "ecdsa-",
    "chacha20-",
    "camellia-",
    "blowfish-",
];

/// Model identifiers of the common hosted model families: `gpt-4o-mini`, `claude-3-5-sonnet`.
const MODEL_PREFIXES: &[&str] = &[
    "gpt-",
    "claude-",
    "gemini-",
    "gemma-",
    "llama-",
    "mistral-",
    "mixtral-",
    "deepseek-",
    "qwen-",
    "command-",
    "whisper-",
    "dall-e-",
    "text-embedding-",
    "o1-",
    "o3-",
    "o4-",
];

/// Date and time format tokens: `hh:mm:ss`, `yyyy-mm-dd`, `dd/mm/yyyy`.
const DATE_FORMAT_PARTS: &[&str] = &[
    "y", "yy", "yyy", "yyyy", "m", "mm", "mmm", "mmmm", "d", "dd", "ddd", "dddd", "h", "hh", "s",
    "ss", "a", "z", "zz",
];

/// Runtime and platform identifiers that are standardised text, not an application's own names:
/// cipher and model names, Node `ERR_*` codes, date-format tokens and cloud regions.
fn is_runtime_identifier(v: &str) -> bool {
    CRYPTO_PREFIXES.iter().any(|p| v.starts_with(p))
        || MODEL_PREFIXES.iter().any(|p| v.starts_with(p))
        || v.starts_with("ERR_")
        || is_date_format(v)
        || is_cloud_region(v)
}

fn is_date_format(v: &str) -> bool {
    let parts: Vec<&str> = v.split([':', '-', '/', '.']).collect();
    parts.len() >= 2 && parts.iter().all(|p| DATE_FORMAT_PARTS.contains(p))
}

/// `us-east-1`, `eu-central-1`, `ap-southeast-2`, `us-gov-west-1`: a two-letter geography, an
/// optional `gov`, a direction word and a number.
fn is_cloud_region(v: &str) -> bool {
    let parts: Vec<&str> = v.split('-').collect();
    let (geo, rest) = match parts.as_slice() {
        [geo, rest @ ..] if rest.len() >= 2 => (*geo, rest),
        _ => return false,
    };
    let (direction, number) = (rest[rest.len() - 2], rest[rest.len() - 1]);
    let middle_ok = match rest.len() {
        2 => true,
        3 => rest[0] == "gov",
        _ => false,
    };
    matches!(
        geo,
        "us" | "eu" | "ap" | "sa" | "ca" | "me" | "af" | "cn" | "il"
    ) && middle_ok
        && !direction.is_empty()
        && direction.bytes().all(|b| b.is_ascii_lowercase())
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

fn is_http_protocol_value(v: &str) -> bool {
    MIME_PREFIXES.iter().any(|p| v.starts_with(p))
        || HTTP_TOKENS.contains(&v)
        || is_locale_tag(v)
        || is_semver(v)
        || is_uuid(v)
        || HEADER_PREFIXES.iter().any(|p| v.starts_with(p))
        || header_name_set().contains(v)
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

/// How a file takes part in the count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileUse {
    /// Read in full: its constants are homes and its other copies are bare.
    Counted,
    /// A configuration file: the constants it defines are homes, but a copy in it is never
    /// counted (per-environment config is SUPPOSED to repeat values).
    HomesOnly,
    /// Tests, fixtures, migrations, scripts, docs, generated code, declaration files and
    /// committed build output: contributes nothing.
    Skipped,
}

/// Whether `path` (with its `content`) is excluded from counting altogether. `emitted` is
/// [`crate::extract::paths::emitted_js_paths`] over the whole repo.
fn is_excluded_file(path: &str, content: &str, emitted: &HashSet<&str>) -> bool {
    classify_file(path, content, emitted) != FileUse::Counted
}

fn classify_file(path: &str, content: &str, emitted: &HashSet<&str>) -> FileUse {
    if emitted.contains(path) || extract::paths::is_test_path(path) {
        return FileUse::Skipped;
    }
    let lower = path.to_ascii_lowercase();
    let mut segments: Vec<&str> = lower.split('/').collect();
    let Some(base) = segments.pop() else {
        return FileUse::Skipped;
    };
    if segments
        .iter()
        .any(|s| GENERATED_DIRS.contains(s) || STANDALONE_DIRS.contains(s))
        || is_declaration_file(base)
        || content
            .lines()
            .take(GENERATED_MARKER_LINES)
            .any(|line| GENERATED_MARKERS.iter().any(|m| line.contains(m)))
    {
        return FileUse::Skipped;
    }
    // F2: root-level files, anything inside a dot directory or named like a dotfile, and
    // configuration directories and file names.
    if segments.is_empty()
        || segments.iter().any(|s| s.starts_with('.'))
        || base.starts_with('.')
        || segments.iter().any(|s| CONFIG_DIRS.contains(s))
        || is_config_shaped_basename(base)
    {
        return FileUse::HomesOnly;
    }
    FileUse::Counted
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

    // ── safe twins ─────────────────────────────────────────────────────────────

    /// A guard proof: `guarded` is silent, and `control` (the same text without the feature the
    /// guard keys on) is flagged exactly once, so the silence is the guard's doing and not an
    /// accident of the fixture.
    fn assert_guarded(guarded: Vec<(&str, &str)>, control: Vec<(&str, &str)>) {
        let silent = run_pairs(guarded);
        assert!(
            silent.is_empty(),
            "guarded input must be silent: {silent:#?}"
        );
        let flagged = run_pairs(control);
        assert_eq!(
            flagged.len(),
            1,
            "control must be flagged once: {flagged:#?}"
        );
    }

    #[test]
    fn twin_same_value_in_two_languages_is_not_grouped() {
        assert_guarded(
            vec![
                ("src/api.ts", "fetch('/api/v1/orders');\n"),
                ("app/server.py", "client.get('/api/v1/orders')\n"),
            ],
            vec![
                ("src/api.ts", "fetch('/api/v1/orders');\n"),
                ("src/other.ts", "fetch('/api/v1/orders');\n"),
            ],
        );
        // Three families, one copy each.
        let v = run_pairs(vec![
            ("src/api.ts", "publish('orders-high');\n"),
            ("app/api.py", "publish('orders-high')\n"),
            ("src/api.rs", "fn f() { publish(\"orders-high\"); }\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_same_literal_twice_in_one_file_is_silent() {
        assert_guarded(
            vec![(
                "src/a.ts",
                "publish('orders-high');\nsubscribe('orders-high');\n",
            )],
            vec![
                ("src/a.ts", "publish('orders-high');\n"),
                ("src/b.ts", "subscribe('orders-high');\n"),
            ],
        );
    }

    #[test]
    fn twin_test_and_fixture_files_are_not_counted() {
        let body = "publish('orders-high');\n";
        let v = run_pairs(vec![
            ("src/a.ts", body),
            ("src/a.test.ts", body),
            ("fixtures/x.ts", body),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // Each test-shaped location on its own, against an ordinary path as the control.
        for test_path in [
            "src/a.spec.ts",
            "src/__tests__/b.ts",
            "e2e/b.ts",
            "src/b.stories.tsx",
            "src/__mocks__/b.ts",
        ] {
            assert_guarded(
                vec![("src/a.ts", body), (test_path, body)],
                vec![("src/a.ts", body), ("src/ordinary.ts", body)],
            );
        }
        let py = "publish('orders-high')\n";
        for test_path in [
            "tests/b.py",
            "app/seeds/b.py",
            "app/conftest.py",
            "app/b_test.py",
            "app/tests.py",
            "app/test_b.py",
        ] {
            assert_guarded(
                vec![("app/a.py", py), (test_path, py)],
                vec![("app/a.py", py), ("app/ordinary.py", py)],
            );
        }
    }

    #[test]
    fn twin_config_and_settings_files_are_not_counted() {
        let py = "CACHES = make({'default': {'LOCATION': 'orders-cache'}})\n";
        let ts = "export default defineConfig({ queue: 'orders-high' });\n";
        // The spec's own pair, with the spec's own value (a header-shaped prefix, so doubly out).
        let v = run_pairs(vec![
            ("config/settings/dev.py", "X = {'L': 'cache-orders'}\n"),
            ("config/settings/prod.py", "X = {'L': 'cache-orders'}\n"),
            ("vite.config.ts", ts),
            ("vitest.config.ts", ts),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        assert_guarded(
            vec![
                ("config/settings/dev.py", py),
                ("config/settings/prod.py", py),
            ],
            vec![("app/dev.py", py), ("app/prod.py", py)],
        );
        assert_guarded(
            vec![("vite.config.ts", ts), ("vitest.config.ts", ts)],
            vec![("src/a.ts", ts), ("src/b.ts", ts)],
        );
        assert_guarded(
            vec![("app/settings.py", py), ("app/settings/prod.py", py)],
            vec![("app/a.py", py), ("app/b.py", py)],
        );
        assert_guarded(
            vec![
                ("src/environments/environment.ts", ts),
                ("src/environments/environment.prod.ts", ts),
            ],
            vec![("src/a.ts", ts), ("src/b.ts", ts)],
        );
    }

    #[test]
    fn twin_migrations_scripts_and_generated_are_not_counted() {
        let py = "register('orders-high')\n";
        let ts = "register('orders-high');\n";
        let v = run_pairs(vec![
            ("migrations/0001.py", py),
            ("migrations/0002.py", py),
            ("scripts/a.ts", ts),
            ("scripts/b.ts", ts),
            ("src/gen.ts", "// @generated\nregister('orders-high');\n"),
            ("src/real.ts", ts),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        assert_guarded(
            vec![("migrations/0001.py", py), ("migrations/0002.py", py)],
            vec![("app/a.py", py), ("app/b.py", py)],
        );
        assert_guarded(
            vec![("scripts/a.ts", ts), ("scripts/b.ts", ts)],
            vec![("src/a.ts", ts), ("src/b.ts", ts)],
        );
        assert_guarded(
            vec![
                ("src/gen.ts", "// @generated\nregister('orders-high');\n"),
                ("src/real.ts", ts),
            ],
            vec![("src/gen.ts", ts), ("src/real.ts", ts)],
        );
        assert_guarded(
            vec![("docs/a.ts", ts), ("examples/b.ts", ts)],
            vec![("src/a.ts", ts), ("src/b.ts", ts)],
        );
    }

    #[test]
    fn twin_declaration_and_emitted_files_are_not_counted() {
        let ts = "register('orders-high');\n";
        assert_guarded(
            vec![("src/types.d.ts", ts), ("src/real.ts", ts)],
            vec![("src/types.ts", ts), ("src/real.ts", ts)],
        );
        // A committed `X.js` beside `X.ts` is build output, not a second module.
        let v = run_pairs(vec![
            ("src/a.ts", "register('orders-high');\n"),
            ("src/a.js", "register('orders-high');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_i18n_key_via_t_call_is_disqualified_everywhere() {
        assert_guarded(
            vec![
                ("src/a.tsx", "const x = t('errors-required');\n"),
                ("src/b.tsx", "const y = label('errors-required');\n"),
                ("src/c.tsx", "const z = label('errors-required');\n"),
            ],
            vec![
                ("src/a.tsx", "const x = pick('errors-required');\n"),
                ("src/b.tsx", "const y = label('errors-required');\n"),
            ],
        );
        for callee in [
            "t",
            "$t",
            "i18n.t",
            "i18next.t",
            "translate",
            "tr",
            "gettext",
            "ngettext",
            "pgettext",
            "_",
            "__",
            "formatMessage",
            "defineMessage",
            "useTranslation",
        ] {
            let a = format!("const x = {callee}('errors-required');\n");
            assert_guarded(
                vec![
                    ("src/a.ts", &a),
                    ("src/b.ts", "label('errors-required');\n"),
                ],
                vec![
                    ("src/a.ts", "pick('errors-required');\n"),
                    ("src/b.ts", "label('errors-required');\n"),
                ],
            );
        }
    }

    #[test]
    fn twin_i18n_key_in_a_python_gettext_call_is_disqualified() {
        assert_guarded(
            vec![
                ("app/a.py", "msg = _('errors-required')\n"),
                ("app/b.py", "label('errors-required')\n"),
                ("app/c.py", "label('errors-required')\n"),
            ],
            vec![
                ("app/b.py", "label('errors-required')\n"),
                ("app/c.py", "label('errors-required')\n"),
            ],
        );
    }

    #[test]
    fn twin_i18n_key_present_in_locales_json_is_disqualified() {
        let a = "label('errors-required');\n";
        assert_guarded(
            vec![
                (
                    "locales/en.json",
                    "{\n  \"errors-required\": \"Required\"\n}\n",
                ),
                ("src/a.ts", a),
                ("src/b.ts", a),
            ],
            vec![
                ("locales/en.json", "{\n  \"other-key\": \"Required\"\n}\n"),
                ("src/a.ts", a),
                ("src/b.ts", a),
            ],
        );
        for catalogue in [
            "src/i18n/en.json",
            "app/lang/en.json",
            "app/langs/en.json",
            "public/translations/en.json",
            "src/messages/en.json",
            "src/intl/en.json",
        ] {
            assert_guarded(
                vec![
                    (catalogue, "{\"errors-required\": \"Required\"}"),
                    ("src/a.ts", a),
                    ("src/b.ts", a),
                ],
                vec![
                    ("src/other/en.json", "{\"errors-required\": \"Required\"}"),
                    ("src/a.ts", a),
                    ("src/b.ts", a),
                ],
            );
        }
    }

    #[test]
    fn twin_i18n_key_in_yaml_catalogues_is_disqualified() {
        let a = "label('errors-required');\n";
        for yaml in [
            "errors-required: Required\n",
            "\"errors-required\": Required\n",
            "'errors-required': Required\n",
            "forms:\n  errors-required: Required\n",
            "- errors-required: Required\n",
        ] {
            assert_guarded(
                vec![("locales/en.yaml", yaml), ("src/a.ts", a), ("src/b.ts", a)],
                vec![
                    ("locales/en.yaml", "other: 1\n"),
                    ("src/a.ts", a),
                    ("src/b.ts", a),
                ],
            );
        }
        assert_guarded(
            vec![
                ("config/locales/en.yml", "errors-required: Required\n"),
                ("src/a.ts", a),
                ("src/b.ts", a),
            ],
            vec![
                ("config/locales/en.yml", "x: 1\n"),
                ("src/a.ts", a),
                ("src/b.ts", a),
            ],
        );
    }

    #[test]
    fn twin_catalogue_values_are_not_keys() {
        // `errors-required` appears only as a VALUE in the catalogue: not an i18n key.
        let a = "label('errors-required');\n";
        let v = run_pairs(vec![
            ("locales/en.json", "{\"message\": \"errors-required\"}"),
            ("src/a.ts", a),
            ("src/b.ts", a),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn twin_class_name_attribute_and_cn_call_are_disqualified() {
        // The spec's pair (`items-center` is also a Tailwind prefix, so doubly out).
        let v = run_pairs(vec![
            (
                "src/a.tsx",
                "export const A = () => <div className=\"items-center\" />;\n",
            ),
            ("src/b.ts", "export const b = cn('items-center');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // A custom class name, which only the position guards can recognise.
        assert_guarded(
            vec![
                (
                    "src/a.tsx",
                    "export const A = () => <div className=\"card-header\" />;\n",
                ),
                ("src/b.ts", "export const x = label('card-header');\n"),
                ("src/c.ts", "export const y = label('card-header');\n"),
            ],
            vec![
                ("src/b.ts", "export const x = label('card-header');\n"),
                ("src/c.ts", "export const y = label('card-header');\n"),
            ],
        );
        for callee in [
            "cn",
            "cx",
            "clsx",
            "classnames",
            "classNames",
            "cva",
            "tv",
            "twMerge",
            "twJoin",
            "tw",
            "css",
            "styled",
            "keyframes",
            "createGlobalStyle",
            "sx",
        ] {
            let a = format!("export const a = {callee}('card-header');\n");
            assert_guarded(
                vec![("src/a.ts", &a), ("src/b.ts", "label('card-header');\n")],
                vec![
                    ("src/a.ts", "pick('card-header');\n"),
                    ("src/b.ts", "label('card-header');\n"),
                ],
            );
        }
        for attr in ["className", "class", "classList", "id", "htmlFor"] {
            let a = format!("export const A = () => <div {attr}=\"card-header\" />;\n");
            assert_guarded(
                vec![("src/a.tsx", &a), ("src/b.ts", "label('card-header');\n")],
                vec![
                    ("src/a.tsx", "export const A = pick('card-header');\n"),
                    ("src/b.ts", "label('card-header');\n"),
                ],
            );
        }
    }

    #[test]
    fn twin_tailwind_prefixed_token_is_disqualified_by_shape() {
        let v = run_pairs(vec![
            ("src/a.ts", "pick('text-gray-500');\n"),
            ("src/b.ts", "pick('text-gray-500');\n"),
            ("src/c.ts", "pick('hover:bg-red-500');\n"),
            ("src/d.ts", "pick('hover:bg-red-500');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_css_keyword_value_under_a_css_property_key_is_excluded() {
        let a = "export const s = { justifyContent: 'space-between' };\n";
        let v = run_pairs(vec![("src/a.ts", a), ("src/b.ts", a)]);
        assert!(v.is_empty(), "{v:#?}");
        // The property key alone excludes the occurrence, whatever the value is (O4).
        assert_guarded(
            vec![
                (
                    "src/a.ts",
                    "export const s = { transition: 'fade-quick' };\n",
                ),
                (
                    "src/b.ts",
                    "export const s = { transition: 'fade-quick' };\n",
                ),
            ],
            vec![
                (
                    "src/a.ts",
                    "export function s() { return { queue: 'fade-quick' }; }\n",
                ),
                (
                    "src/b.ts",
                    "export function s() { return { queue: 'fade-quick' }; }\n",
                ),
            ],
        );
        // kebab-case property keys too.
        assert_guarded(
            vec![
                (
                    "app/a.py",
                    "def f():\n    return {'transition': 'fade-quick'}\n",
                ),
                (
                    "app/b.py",
                    "def f():\n    return {'transition': 'fade-quick'}\n",
                ),
            ],
            vec![
                ("app/a.py", "def f():\n    return {'queue': 'fade-quick'}\n"),
                ("app/b.py", "def f():\n    return {'queue': 'fade-quick'}\n"),
            ],
        );
    }

    #[test]
    fn twin_env_var_name_is_disqualified() {
        let ts = vec![
            ("src/a.ts", "const q = process.env['ORDERS_QUEUE'];\n"),
            ("src/b.ts", "register('ORDERS_QUEUE');\n"),
            ("src/c.ts", "register('ORDERS_QUEUE');\n"),
        ];
        assert_guarded(
            ts,
            vec![
                ("src/b.ts", "register('ORDERS_QUEUE');\n"),
                ("src/c.ts", "register('ORDERS_QUEUE');\n"),
            ],
        );
        assert_guarded(
            vec![
                ("app/a.py", "q = os.environ['ORDERS_QUEUE']\n"),
                ("app/b.py", "register('ORDERS_QUEUE')\n"),
                ("app/c.py", "register('ORDERS_QUEUE')\n"),
            ],
            vec![
                ("app/b.py", "register('ORDERS_QUEUE')\n"),
                ("app/c.py", "register('ORDERS_QUEUE')\n"),
            ],
        );
        for first in [
            ("src/a.ts", "const q = getenv('ORDERS_QUEUE');\n"),
            ("src/a.ts", "const q = env('ORDERS_QUEUE');\n"),
            ("src/a.ts", "const q = config.env('ORDERS_QUEUE');\n"),
            ("src/a.ts", "const c = { env: 'ORDERS_QUEUE' };\n"),
            ("app/a.py", "q = os.getenv('ORDERS_QUEUE')\n"),
            ("app/a.py", "q = os.environ.get('ORDERS_QUEUE')\n"),
            (
                "src/a.rs",
                "fn f() { let q = std::env::var(\"ORDERS_QUEUE\"); }\n",
            ),
            (
                "src/a.rs",
                "fn f() { let q = env::var_os(\"ORDERS_QUEUE\"); }\n",
            ),
        ] {
            let family_ts = first.0.ends_with(".ts");
            let family_py = first.0.ends_with(".py");
            let (b, c) = if family_ts {
                ("src/b.ts", "src/c.ts")
            } else if family_py {
                ("app/b.py", "app/c.py")
            } else {
                ("src/b.rs", "src/c.rs")
            };
            let body = if family_ts {
                "register('ORDERS_QUEUE');\n"
            } else if family_py {
                "register('ORDERS_QUEUE')\n"
            } else {
                "fn f() { register(\"ORDERS_QUEUE\"); }\n"
            };
            let silent = run_pairs(vec![first, (b, body), (c, body)]);
            assert!(silent.is_empty(), "{first:?}: {silent:#?}");
        }
        // The env-name guard is for S3 values only: a kebab value under `env(..)` is not one.
        let v = run_pairs(vec![
            ("src/a.ts", "env('orders-high');\n"),
            ("src/b.ts", "register('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn twin_screaming_snake_member_of_a_union_type_is_disqualified() {
        assert_guarded(
            vec![
                (
                    "src/types.ts",
                    "export type Status = 'IN_PROGRESS' | 'DONE';\n",
                ),
                ("src/a.ts", "if (s === 'IN_PROGRESS') {}\n"),
                ("src/b.ts", "if (s === 'IN_PROGRESS') {}\n"),
            ],
            vec![
                ("src/types.ts", "export type Status = 'DONE';\n"),
                ("src/a.ts", "if (s === 'IN_PROGRESS') {}\n"),
                ("src/b.ts", "if (s === 'IN_PROGRESS') {}\n"),
            ],
        );
        // An `as const` tuple that a type is derived from is a type source too.
        assert_guarded(
            vec![
                (
                    "src/types.ts",
                    "export const STATUSES = ['IN_PROGRESS', 'DONE'] as const;\nexport type Status = (typeof STATUSES)[number];\n",
                ),
                ("src/a.ts", "if (s === 'IN_PROGRESS') {}\n"),
                ("src/b.ts", "if (s === 'IN_PROGRESS') {}\n"),
            ],
            vec![
                ("src/types.ts", "export const STATUSES = ['DONE'] as const;\n"),
                ("src/a.ts", "if (s === 'IN_PROGRESS') {}\n"),
                ("src/b.ts", "if (s === 'IN_PROGRESS') {}\n"),
            ],
        );
        // Kebab members of a union are S1 values: the same guard.
        assert_guarded(
            vec![
                (
                    "src/types.ts",
                    "export type Lane = 'orders-high' | 'orders-low';\n",
                ),
                ("src/a.ts", "publish('orders-high');\n"),
                ("src/b.ts", "publish('orders-high');\n"),
            ],
            vec![
                ("src/types.ts", "export type Lane = 'orders-low';\n"),
                ("src/a.ts", "publish('orders-high');\n"),
                ("src/b.ts", "publish('orders-high');\n"),
            ],
        );
    }

    #[test]
    fn twin_enum_initializer_is_disqualified() {
        assert_guarded(
            vec![
                (
                    "src/types.ts",
                    "export enum S { InProgress = 'IN_PROGRESS' }\n",
                ),
                ("src/a.ts", "if (s === 'IN_PROGRESS') {}\n"),
                ("src/b.ts", "if (s === 'IN_PROGRESS') {}\n"),
            ],
            vec![
                (
                    "src/types.ts",
                    "export const S = { InProgress: 'IN_PROGRESS' };\n",
                ),
                ("src/a.ts", "if (s === 'IN_PROGRESS') {}\n"),
                ("src/b.ts", "if (s === 'IN_PROGRESS') {}\n"),
            ],
        );
    }

    #[test]
    fn twin_python_literal_and_enum_are_disqualified() {
        let uses = |first: &'static str| {
            vec![
                ("app/types.py", first),
                ("app/a.py", "if s == 'IN_PROGRESS':\n    pass\n"),
                ("app/b.py", "if s == 'IN_PROGRESS':\n    pass\n"),
            ]
        };
        let control = vec![
            ("app/a.py", "if s == 'IN_PROGRESS':\n    pass\n"),
            ("app/b.py", "if s == 'IN_PROGRESS':\n    pass\n"),
        ];
        assert_guarded(uses("Status = Literal['IN_PROGRESS']\n"), control.clone());
        assert_guarded(
            uses("class Status(Enum):\n    IN_PROGRESS = 'IN_PROGRESS'\n"),
            control,
        );
    }

    #[test]
    fn twin_import_specifier_shaped_string_is_disqualified() {
        // The spec's pair: an import and a test mock. Neither is a bare copy of a constant.
        let v = run_pairs(vec![
            ("src/a.ts", "import x from 'react-dom/client';\n"),
            ("src/b.ts", "jest.mock('react-dom/client');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // And a specifier anywhere removes the value from the other users.
        assert_guarded(
            vec![
                ("src/a.ts", "import x from 'react-dom/client';\n"),
                ("src/b.ts", "label('react-dom/client');\n"),
                ("src/c.ts", "label('react-dom/client');\n"),
            ],
            vec![
                ("src/b.ts", "label('react-dom/client');\n"),
                ("src/c.ts", "label('react-dom/client');\n"),
            ],
        );
        for first in [
            "const x = require('react-dom/client');\n",
            "const x = await import('react-dom/client');\n",
            "export * from 'react-dom/client';\n",
            "vi.mock('react-dom/client');\n",
            "require.resolve('react-dom/client');\n",
        ] {
            let silent = run_pairs(vec![
                ("src/a.ts", first),
                ("src/b.ts", "label('react-dom/client');\n"),
                ("src/c.ts", "label('react-dom/client');\n"),
            ]);
            assert!(silent.is_empty(), "{first}: {silent:#?}");
        }
        assert_guarded(
            vec![
                ("app/a.py", "m = importlib.import_module('app/plugins')\n"),
                ("app/b.py", "label('app/plugins')\n"),
                ("app/c.py", "label('app/plugins')\n"),
            ],
            vec![
                ("app/b.py", "label('app/plugins')\n"),
                ("app/c.py", "label('app/plugins')\n"),
            ],
        );
    }

    #[test]
    fn twin_mime_header_encoding_and_cache_tokens_are_excluded() {
        let body = "res.setHeader('content-type', 'application/json');\n\
                    res.setHeader('x-request-id', id);\n\
                    enc('utf-8');\ncache('no-cache');\nmode('same-origin');\n\
                    locale('en-us');\nver('1.2.0-beta');\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_header_call_receivers_disqualify_a_value_that_has_no_header_shape() {
        for first in [
            "res.setHeader('tenant-id', v);\n",
            "const v = req.getHeader('tenant-id');\n",
            "res.header('tenant-id', v);\n",
            "res.headers.set('tenant-id', v);\n",
            "res.headers.append('tenant-id', v);\n",
            "const v = res.headers.get('tenant-id');\n",
            "res.headers.has('tenant-id');\n",
            "res.headers.delete('tenant-id');\n",
            "res.headers.insert('tenant-id', v);\n",
        ] {
            let silent = run_pairs(vec![
                ("src/a.ts", first),
                ("src/b.ts", "label('tenant-id');\n"),
                ("src/c.ts", "label('tenant-id');\n"),
            ]);
            assert!(silent.is_empty(), "{first}: {silent:#?}");
        }
        // The same verbs on a receiver that is not a headers object are ordinary calls.
        let v = run_pairs(vec![
            ("src/a.ts", "cache.set('tenant-id', v);\n"),
            ("src/b.ts", "store.get('tenant-id');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_guarded(
            vec![
                ("app/a.py", "v = response.headers.get('tenant-id')\n"),
                ("app/b.py", "label('tenant-id')\n"),
                ("app/c.py", "label('tenant-id')\n"),
            ],
            vec![
                ("app/b.py", "label('tenant-id')\n"),
                ("app/c.py", "label('tenant-id')\n"),
            ],
        );
    }

    #[test]
    fn twin_platform_tokens_are_excluded() {
        let body = "t('linux-x64');\ntarget('wasm32-unknown-unknown');\nattr('aria-label');\nattr('data-testid');\narch('x86_64');\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_host_port_date_ip_and_uuid_fail_the_shape() {
        let body = "f('localhost:3000');\nf('2024-01-01');\nf('127.0.0.1');\nf('0.0.0.0');\n\
                    f('123e4567-e89b-12d3-a456-426614174000');\nf('db:5432');\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_filesystem_paths_and_urls_are_excluded() {
        let body = "f('./data/seed.json');\nf('/tmp/cache');\nf('config/default.yaml');\n\
                    f('https://api.example.com/v1');\nf('/usr/local/bin');\nf('../shared/lib');\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_object_keys_and_subscripts_are_not_counted() {
        // The spec's pair (`x-` is also a header prefix, so doubly out).
        let v = run_pairs(vec![
            ("src/a.ts", "const h = { 'x-tenant': 1 };\n"),
            ("src/b.ts", "const t = row['x-tenant'];\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        assert_guarded(
            vec![
                ("src/a.ts", "const h = { 'tenant-id': 1 };\n"),
                ("src/b.ts", "const t = row['tenant-id'];\n"),
            ],
            vec![
                ("src/a.ts", "const h = { id: 'tenant-id' };\n"),
                ("src/b.ts", "const t = pick('tenant-id');\n"),
            ],
        );
        assert_guarded(
            vec![
                ("app/a.py", "h = {'tenant-id': 1}\n"),
                ("app/b.py", "t = row['tenant-id']\n"),
            ],
            vec![
                ("app/a.py", "h = {'id': 'tenant-id'}\n"),
                ("app/b.py", "t = pick('tenant-id')\n"),
            ],
        );
        assert_guarded(
            vec![
                ("src/a.rs", "fn f() { let t = row[\"tenant-id\"]; }\n"),
                ("src/b.rs", "fn f() { let t = row[\"tenant-id\"]; }\n"),
            ],
            vec![
                ("src/a.rs", "fn f() { let t = pick(\"tenant-id\"); }\n"),
                ("src/b.rs", "fn f() { let t = pick(\"tenant-id\"); }\n"),
            ],
        );
    }

    #[test]
    fn twin_dotted_only_and_snake_only_strings_are_out_of_scope() {
        let body = "f('orders.created');\nf('order_created');\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_single_word_strings_are_out_of_scope() {
        let body = "f('pending');\nf('orders');\nf('GET');\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_logger_name_is_disqualified() {
        for first in [
            "const l = getLogger('svc-orders');\n",
            "const l = logging.getLogger('svc-orders');\n",
            "const l = get_logger('svc-orders');\n",
            "const l = log.child('svc-orders');\n",
            "const l = logger('svc-orders');\n",
            "const s = span('svc-orders');\n",
            "const s = tracer.instrument('svc-orders');\n",
            "const s = trace_span('svc-orders');\n",
            "const s = info_span('svc-orders');\n",
            "const s = debug_span('svc-orders');\n",
        ] {
            let silent = run_pairs(vec![
                ("src/a.ts", first),
                ("src/b.ts", "tag('svc-orders');\n"),
            ]);
            assert!(silent.is_empty(), "{first}: {silent:#?}");
        }
        assert_guarded(
            vec![
                ("app/a.py", "log = logging.getLogger('svc-orders')\n"),
                ("app/b.py", "tag('svc-orders')\n"),
            ],
            vec![
                ("app/a.py", "tag('svc-orders')\n"),
                ("app/b.py", "tag('svc-orders')\n"),
            ],
        );
        assert_guarded(
            vec![
                (
                    "src/a.rs",
                    "fn f() { let s = info_span(\"svc-orders\"); }\n",
                ),
                ("src/b.rs", "fn f() { tag(\"svc-orders\"); }\n"),
            ],
            vec![
                ("src/a.rs", "fn f() { tag(\"svc-orders\"); }\n"),
                ("src/b.rs", "fn f() { tag(\"svc-orders\"); }\n"),
            ],
        );
    }

    #[test]
    fn twin_dom_event_names_are_excluded_but_custom_channels_count() {
        let v = run_pairs(vec![
            ("src/a.ts", "el.addEventListener('click', h);\n"),
            ("src/b.ts", "el.addEventListener('click', h);\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        let v = run_pairs(vec![
            ("src/a.ts", "bus.on('order:created', h);\n"),
            ("src/b.ts", "bus.emit('order:created');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert_eq!(v[0].object.as_deref(), Some("order:created"));
    }

    #[test]
    fn v11_event_names_under_event_callees_disqualify_a_value() {
        // Real DOM and Node events are single words today, so the shape already drops them; the
        // guard is pinned directly so a future widening of the shape cannot reopen it.
        let ctx_for = |callee: &str| LiteralCtx {
            callee: Some(callee.to_string()),
            callee_chain: Some(callee.to_string()),
            ..LiteralCtx::default()
        };
        for callee in [
            "addEventListener",
            "removeEventListener",
            "on",
            "once",
            "off",
            "emit",
        ] {
            for event in [
                "click",
                "keydown",
                "SIGTERM",
                "uncaughtException",
                "message",
            ] {
                assert!(
                    context_disqualifies(Shape::Token, event, &ctx_for(callee)),
                    "{callee} {event}"
                );
            }
            assert!(
                !context_disqualifies(Shape::Token, "order:created", &ctx_for(callee)),
                "{callee} custom"
            );
        }
        let process_on = LiteralCtx {
            callee: Some("on".to_string()),
            callee_chain: Some("process.on".to_string()),
            ..LiteralCtx::default()
        };
        assert!(context_disqualifies(Shape::Token, "SIGINT", &process_on));
        assert!(!context_disqualifies(
            Shape::Token,
            "click",
            &ctx_for("publish")
        ));
    }

    #[test]
    fn twin_serde_rename_and_cfg_feature_attributes_are_excluded() {
        let rename = "#[derive(Serialize)]\nstruct S {\n    #[serde(rename = \"created-at\")]\n    a: u8,\n}\n";
        let v = run_pairs(vec![("src/a.rs", rename), ("src/b.rs", rename)]);
        assert!(v.is_empty(), "{v:#?}");
        let cfg = "#[cfg(feature = \"extra-fast\")]\nfn f() {}\n";
        let v = run_pairs(vec![("src/a.rs", cfg), ("src/b.rs", cfg)]);
        assert!(v.is_empty(), "{v:#?}");
        // The same text as an ordinary string is flagged.
        let plain = "fn f() { pick(\"created-at\"); }\n";
        let v = run_pairs(vec![("src/a.rs", plain), ("src/b.rs", plain)]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn twin_python_decorator_arguments_that_are_not_routes_are_excluded() {
        let task = "@celery.task(queue='orders-high')\ndef work():\n    pass\n";
        assert_guarded(
            vec![("app/a.py", task), ("app/b.py", task)],
            vec![
                ("app/a.py", "work(queue='orders-high')\n"),
                ("app/b.py", "work(queue='orders-high')\n"),
            ],
        );
    }

    #[test]
    fn twin_rust_macro_bodies_are_not_extracted() {
        let body = "fn f() { let j = json!({\"q\": \"orders-high\"}); }\n";
        let v = run_pairs(vec![("src/a.rs", body), ("src/b.rs", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_interpolated_and_concatenated_strings_are_skipped() {
        let ts = "const u = `/api/${v}`;\nconst q = 'orders-' + tier;\n";
        let v = run_pairs(vec![("src/a.ts", ts), ("src/b.ts", ts)]);
        assert!(v.is_empty(), "{v:#?}");
        let py = "u = f'/api/{v}'\nq = 'orders-' 'high'\n";
        let v = run_pairs(vec![("app/a.py", py), ("app/b.py", py)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_decimal_outside_arithmetic_is_not_counted() {
        let v = run_pairs(vec![
            ("src/a.ts", "export const s = { opacity: 0.8 };\n"),
            ("src/b.ts", "export const s = { scale: 0.8 };\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // The excluded round fractions, even in arithmetic.
        for fraction in ["0.5", "0.25", "0.75"] {
            let a = format!("const t = total * {fraction};\n");
            let v = run_pairs(vec![("src/a.ts", &a), ("src/b.ts", &a)]);
            assert!(v.is_empty(), "{fraction}: {v:#?}");
        }
        // Every bare copy must be an operand: one stray copy removes the value.
        assert_guarded(
            vec![
                ("src/a.ts", "const t = total * 0.12;\n"),
                ("src/b.ts", "const t = subtotal * 0.12;\n"),
                (
                    "src/c.ts",
                    "function f() { const fee = 0.12; return fee; }\n",
                ),
            ],
            vec![
                ("src/a.ts", "const t = total * 0.12;\n"),
                ("src/b.ts", "const t = subtotal * 0.12;\n"),
            ],
        );
    }

    #[test]
    fn decimal_rates_work_in_python_and_rust_arithmetic() {
        let v = run_pairs(vec![
            ("app/a.py", "t = total * 0.12\n"),
            ("app/b.py", "t = subtotal * 0.12\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        let v = run_pairs(vec![
            ("src/a.rs", "fn f(t: f64) -> f64 { t * 0.12 }\n"),
            ("src/b.rs", "fn g(t: f64) -> f64 { t + t * 0.12 }\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn a_rate_with_a_home_flags_each_arithmetic_copy() {
        let v = run_pairs(vec![
            ("src/fees.ts", "export const FEE_RATE = 0.12;\n"),
            ("src/cart.ts", "const t = total * 0.12;\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`FEE_RATE`"), "{}", v[0].message);
        // A non-arithmetic bare copy of the same number removes it.
        let v = run_pairs(vec![
            ("src/fees.ts", "export const FEE_RATE = 0.12;\n"),
            ("src/cart.ts", "const t = total * 0.12;\n"),
            (
                "src/ui.ts",
                "const s = { opacity: 0.12 };\nconst x = pick(0.12);\n",
            ),
        ]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_integers_and_time_units_are_out_of_scope() {
        let body = "const a = t * 60;\nconst b = t * 1000;\nconst c = t * 86400;\n\
                    const d = t * 1048576;\nconst e = t + 404;\nconst f = t + 8080;\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_two_homes_and_no_bare_copies_is_silent() {
        let v = run_pairs(vec![
            ("src/a.ts", "export const Q = 'orders-high';\n"),
            ("src/b.ts", "export const Q = 'orders-high';\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn two_homes_with_a_bare_copy_name_both_homes() {
        let v = run_pairs(vec![
            ("src/a.ts", "export const QA = 'orders-high';\n"),
            ("src/b.ts", "export const QB = 'orders-high';\n"),
            ("src/c.ts", "subscribe('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        let m = &v[0].message;
        for needle in [
            "`QA` in `src/a.ts:1`",
            "`QB` in `src/b.ts:1`",
            "1 other file",
        ] {
            assert!(m.contains(needle), "{needle} missing: {m}");
        }
    }

    #[test]
    fn twin_home_file_own_bare_use_is_ignored() {
        let v = run_pairs(vec![
            (
                "src/q.ts",
                "export const Q = 'orders-high';\nlog('orders-high');\n",
            ),
            ("src/other.ts", "subscribe('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("1 other file"), "{}", v[0].message);
        // With no other file there is nothing to flag.
        let v = run_pairs(vec![(
            "src/q.ts",
            "export const Q = 'orders-high';\nlog('orders-high');\n",
        )]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn twin_sql_fragment_callees_are_excluded() {
        for callee in [
            "sql", "raw", "query", "execute", "prepare", "select", "insert", "update", "delete",
            "where", "orderBy", "groupBy",
        ] {
            let first = format!("db.{callee}('orders-high');\n");
            let v = run_pairs(vec![
                ("src/a.ts", &first),
                ("src/b.ts", "publish('orders-high');\n"),
            ]);
            assert!(v.is_empty(), "{callee}: {v:#?}");
        }
        // The spec's pair, and the callees that ARE the rule's target.
        let v = run_pairs(vec![
            ("src/a.ts", "query('orders-high');\n"),
            ("src/b.ts", "raw('orders-high');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        for callee in [
            "from",
            "table",
            "collection",
            "model",
            "rpc",
            "channel",
            "bucket",
            "topic",
            "queue",
        ] {
            let a = format!("db.{callee}('orders-high');\n");
            let v = run_pairs(vec![
                ("src/a.ts", &a),
                ("src/b.ts", "publish('orders-high');\n"),
            ]);
            assert_eq!(v.len(), 1, "{callee}: {v:#?}");
        }
    }

    #[test]
    fn value_level_guards_ignore_excluded_files() {
        // The i18n call sits in a test file, which is not read: it cannot disqualify the value.
        let v = run_pairs(vec![
            ("src/a.test.tsx", "const x = t('errors-required');\n"),
            ("src/b.ts", "label('errors-required');\n"),
            ("src/c.ts", "label('errors-required');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn a_guard_in_one_language_family_does_not_reach_another() {
        // `t('errors-required')` in Python does not disqualify the TypeScript copies.
        let v = run_pairs(vec![
            ("app/a.py", "msg = _('errors-required')\n"),
            ("src/b.ts", "label('errors-required');\n"),
            ("src/c.ts", "label('errors-required');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn twin_empty_and_malformed_inputs_never_panic() {
        let _ = run_pairs(vec![
            ("src/a.ts", ""),
            ("src/b.ts", "\u{0}\u{1}\u{2}\u{ffff}"),
            ("src/c.rs", "fn f( { let = ;"),
            ("src/d.tsx", "<div className=\"a-b"),
            ("app/e.py", "def (:\n x = 'orders-high"),
            ("locales/en.json", "{ not json"),
            ("locales/en.yaml", ": : :\n- - -\n"),
            ("src/f.ts", "const a = `${"),
        ]);
        assert!(run_pairs(vec![]).is_empty());
        assert!(run_pairs(vec![("README.md", "orders-high"), ("a.txt", "orders-high")]).is_empty());
    }

    #[test]
    fn message_is_client_voice_without_dashes_or_tool_names() {
        let v = run_pairs(vec![
            (
                "src/queues.ts",
                "export const ORDERS_QUEUE = 'orders-high';\n",
            ),
            ("src/a.ts", "subscribe('orders-high');\n"),
            ("src/b.ts", "publish('orders-high');\n"),
        ]);
        let plain = run_pairs(vec![
            ("src/a.ts", "subscribe('orders-high');\n"),
            ("src/b.ts", "publish('orders-high');\n"),
        ]);
        for m in [&v[0].message, &plain[0].message] {
            assert!(!m.contains('\u{2014}') && !m.contains('\u{2013}'), "{m}");
            for banned in [
                "camerata", "Camerata", "eslint", "ESLint", "Sonar", "Semgrep",
            ] {
                assert!(!m.contains(banned), "{banned} in {m}");
            }
        }
    }

    // ── review fixes: test code ────────────────────────────────────────────────

    #[test]
    fn python_test_modules_are_not_counted_cases_17_18_51_98() {
        // 17: Django's tests.py exercising the route the app also uses.
        let v = run_pairs(vec![
            (
                "orders/client.py",
                "def fetch(c):\n    return c.get('/api/v1/orders')\n",
            ),
            (
                "orders/tests.py",
                "def t(c):\n    c.get('/api/v1/orders')\n",
            ),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // 18: pytest's test_*.py.
        let v = run_pairs(vec![
            (
                "orders/client.py",
                "def fetch(c):\n    return c.get('/api/v1/orders')\n",
            ),
            (
                "orders/test_client.py",
                "def test_x(c):\n    c.get('/api/v1/orders')\n",
            ),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // 51: a status literal asserted in tests.py.
        let v = run_pairs(vec![
            (
                "orders/services.py",
                "def status(x):\n    return 'in-progress'\n",
            ),
            (
                "orders/tests.py",
                "def test_a():\n    assert status('x') == 'in-progress'\n",
            ),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // 98: tests.py, a tests/ directory and one real use: one copy.
        let c = "c.get('/api/v1/orders')\n";
        let v = run_pairs(vec![
            ("orders/tests.py", c),
            ("orders/tests/test_x.py", c),
            ("orders/x.py", c),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // The control: two ordinary modules are flagged.
        let v = run_pairs(vec![("orders/x.py", c), ("orders/y.py", c)]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn rust_test_only_code_is_not_counted_cases_27_56_78() {
        // 27: a value asserted inside an inline test module.
        let v = run_pairs(vec![
            ("src/api.rs", "pub fn publish_all() { publish(\"orders-high\"); }\n"),
            (
                "src/worker.rs",
                "pub fn run() { subscribe(Q); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { let expected = \"orders-high\"; assert!(expected.len() > 0); }\n}\n",
            ),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // 56: two files whose only copy is inside a test module.
        let t = "#[cfg(test)]\nmod tests { fn t() { let q = \"orders-high\"; } }\n";
        let v = run_pairs(vec![("src/a.rs", t), ("src/b.rs", t)]);
        assert!(v.is_empty(), "{v:#?}");
        // 78: a constant defined in a test module is not the application's home.
        let v = run_pairs(vec![
            (
                "src/a.rs",
                "#[cfg(test)]\nmod tests { const Q: &str = \"orders-high\"; }\n",
            ),
            ("src/b.rs", "fn b() { publish(\"orders-high\"); }\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // The control: the same copies outside test code are flagged.
        let plain = "fn t() { let q = \"orders-high\"; }\n";
        let v = run_pairs(vec![("src/a.rs", plain), ("src/b.rs", plain)]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    // ── review fixes: homes ────────────────────────────────────────────────────

    /// Every shape of module-level home the extractors model, as (path, source) in one language
    /// family, for the value `orders-high` (or a route for an enum, whose S1 members the type
    /// itself owns).
    fn home_shapes() -> Vec<(&'static str, &'static str, &'static str)> {
        // (path a, path b, source)
        vec![
            ("src/a.ts", "src/b.ts", "export const Q = 'orders-high';\n"),
            ("src/a.js", "src/b.js", "export const QUEUES = Object.freeze({ ORDERS: 'orders-high' });\n"),
            ("src/a.ts", "src/b.ts", "export default { ORDERS: 'orders-high' };\n"),
            ("src/a.js", "src/b.js", "module.exports = { ORDERS: 'orders-high' };\n"),
            ("src/a.js", "src/b.js", "exports.ORDERS = 'orders-high';\n"),
            ("src/a.ts", "src/b.ts", "export namespace Q { export const ORDERS = 'orders-high'; }\n"),
            ("src/a.ts", "src/b.ts", "export class Q { static ORDERS = 'orders-high'; }\n"),
            ("app/a.py", "app/b.py", "ORDERS_QUEUE = 'orders-high'\n"),
            ("app/a.py", "app/b.py", "class Queues:\n    ORDERS = 'orders-high'\n"),
            ("src/a.rs", "src/b.rs", "pub const Q: &str = \"orders-high\";\n"),
            (
                "src/a.rs",
                "src/b.rs",
                "pub static Q: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| \"orders-high\".to_string());\n",
            ),
            (
                "src/a.rs",
                "src/b.rs",
                "lazy_static! { pub static ref Q: String = \"orders-high\".to_string(); }\n",
            ),
            ("src/a.rs", "src/b.rs", "pub fn queue() -> &'static str { \"orders-high\" }\n"),
        ]
    }

    #[test]
    fn twin_two_homes_and_no_bare_copies_is_silent_for_every_home_shape() {
        for (a, b, src) in home_shapes() {
            let v = run_pairs(vec![(a, src), (b, src)]);
            assert!(v.is_empty(), "{src}: {v:#?}");
            // A bare copy in a third file is reported against the homes, never as "give it one home".
            let ext = a.rsplit('.').next().unwrap();
            let bare = match ext {
                "py" => "subscribe('orders-high')\n",
                "rs" => "fn w() { subscribe(\"orders-high\"); }\n",
                _ => "subscribe('orders-high');\n",
            };
            let third = format!("src/c.{ext}");
            let third = if ext == "py" {
                "app/c.py".to_string()
            } else {
                third
            };
            let v = run_pairs(vec![(a, src), (b, src), (&third, bare)]);
            assert_eq!(v.len(), 1, "{src}: {v:#?}");
            assert!(
                v[0].message.contains("is defined as the constants"),
                "{src}: {}",
                v[0].message
            );
            assert!(
                !v[0].message.contains("Give it one home"),
                "{}",
                v[0].message
            );
        }
    }

    #[test]
    fn a_single_home_of_every_shape_is_named_in_the_finding_cases_23_75_84_92() {
        // 84: a class attribute, then a bare use.
        let v = run_pairs(vec![
            (
                "app/queues.py",
                "class Queues:\n    ORDERS = 'orders-high'\n",
            ),
            ("app/worker.py", "def w():\n    subscribe('orders-high')\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`Queues.ORDERS`"), "{}", v[0].message);
        // 92: Object.freeze.
        let v = run_pairs(vec![
            (
                "src/queues.js",
                "export const QUEUES = Object.freeze({ ORDERS: 'orders-high' });\n",
            ),
            ("src/worker.js", "subscribe('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`QUEUES`"), "{}", v[0].message);
        // 23: an enum member that holds a route.
        let v = run_pairs(vec![
            (
                "src/routes.ts",
                "export enum Routes { Orders = '/api/orders' }\n",
            ),
            ("src/a.ts", "fetch('/api/orders');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`Routes.Orders`"), "{}", v[0].message);
        // 24: two enums with the same route, no bare copy.
        let e = "export enum Routes { Orders = '/api/orders' }\n";
        let v = run_pairs(vec![("src/a.ts", e), ("src/b.ts", e)]);
        assert!(v.is_empty(), "{v:#?}");
        // 85: a namespace member.
        let v = run_pairs(vec![
            (
                "src/q.ts",
                "export namespace Q { export const ORDERS = 'orders-high'; }\n",
            ),
            ("src/w.ts", "subscribe('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(v[0].message.contains("`Q.ORDERS`"), "{}", v[0].message);
    }

    #[test]
    fn django_choices_and_typed_enum_members_are_still_owned_by_their_type_cases_74_75_79() {
        let choices =
            "class Status(models.TextChoices):\n    IN_PROGRESS = 'in-progress', 'In progress'\n";
        let v = run_pairs(vec![
            ("billing/models.py", choices),
            ("orders/models.py", choices),
        ]);
        assert!(v.is_empty(), "74: {v:#?}");
        let v = run_pairs(vec![
            ("orders/models.py", choices),
            (
                "orders/views.py",
                "def v():\n    return Order.objects.filter(status='in-progress')\n",
            ),
        ]);
        assert!(v.is_empty(), "75: {v:#?}");
        let e = "export const enum K { A = 'ORDERS_HIGH' }\n";
        let v = run_pairs(vec![("src/a.ts", e), ("src/b.ts", e)]);
        assert!(v.is_empty(), "79: {v:#?}");
    }

    #[test]
    fn unattributed_class_bodies_are_neither_home_nor_bare() {
        // A class the walker cannot name: not a home, and not a copy either.
        let nested = "def f():\n    class C:\n        Q = 'orders-high'\n";
        let v = run_pairs(vec![
            ("app/a.py", nested),
            ("app/b.py", "subscribe('orders-high')\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        let v = run_pairs(vec![
            (
                "src/a.ts",
                "function f() { class C { static Q = 'orders-high'; } }\n",
            ),
            ("src/b.ts", "subscribe('orders-high');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn config_files_contribute_homes_but_never_bare_copies_case_58() {
        let v = run_pairs(vec![
            ("src/api.ts", "publish('orders-high');\n"),
            (
                "src/config/queues.ts",
                "export const ORDERS_QUEUE = 'orders-high';\n",
            ),
            ("src/worker.ts", "subscribe('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        let m = &v[0].message;
        assert!(
            m.contains("defined once as the constant `ORDERS_QUEUE`"),
            "{m}"
        );
        assert!(m.contains("src/config/queues.ts:1"), "{m}");
        assert!(!m.contains("Give it one home"), "{m}");
        // A bare copy in a config file is still not counted.
        let v = run_pairs(vec![
            ("src/config/a.ts", "publish('orders-high');\n"),
            ("src/b.ts", "publish('orders-high');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // A root-level file's constant is a home too; its bare copies are not counted.
        let v = run_pairs(vec![
            ("constants.ts", "export const Q = 'orders-high';\n"),
            ("src/b.ts", "publish('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn a_constant_in_a_test_file_is_not_a_home_case_97() {
        let v = run_pairs(vec![
            ("src/a.test.ts", "export const Q = 'orders-high';\n"),
            ("src/a.ts", "publish('orders-high');\n"),
            ("src/b.ts", "subscribe('orders-high');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        assert!(
            v[0].message.contains("bare literal in 2 files"),
            "{}",
            v[0].message
        );
        assert!(!v[0].message.contains("a.test.ts"), "{}", v[0].message);
    }

    // ── review fixes: environment variable names ───────────────────────────────

    #[test]
    fn env_var_names_read_through_any_environment_receiver_cases_11_12_13_14_15_73() {
        let two = |a: (&str, &str), b: (&str, &str)| run_pairs(vec![a, b]);
        // 11: os.environ.setdefault.
        let line = "os.environ.setdefault('DJANGO_SETTINGS_MODULE', 'proj.settings')\n";
        let v = run_pairs(vec![
            ("proj/asgi.py", line),
            ("proj/wsgi.py", line),
            ("proj/celery.py", line),
        ]);
        assert!(v.is_empty(), "11: {v:#?}");
        // 12 and 73: Django's request.META, by subscript and by .get.
        let v = two(
            (
                "app/utils.py",
                "def f(r):\n    return r.META['HTTP_X_FORWARDED_FOR']\n",
            ),
            (
                "app/views.py",
                "def g(r):\n    return r.META.get('HTTP_X_FORWARDED_FOR')\n",
            ),
        );
        assert!(v.is_empty(), "12: {v:#?}");
        let g = "def g(r):\n    return r.META.get('HTTP_X_FORWARDED_FOR')\n";
        let v = two(("app/utils.py", g), ("app/views.py", g));
        assert!(v.is_empty(), "73: {v:#?}");
        // 13: Deno.env.get.
        let d = "export const u = Deno.env.get('DATABASE_URL_RO');\n";
        let v = two(("src/db.ts", d), ("src/jobs.ts", d));
        assert!(v.is_empty(), "13: {v:#?}");
        // 14: std::env::set_var, and remove_var.
        let v = two(
            (
                "src/bin/worker.rs",
                "fn main() { std::env::set_var(\"RUST_LOG\", \"debug\"); }\n",
            ),
            (
                "src/main.rs",
                "fn main() { std::env::set_var(\"RUST_LOG\", \"info\"); }\n",
            ),
        );
        assert!(v.is_empty(), "14: {v:#?}");
        let r = "fn main() { std::env::remove_var(\"RUST_LOG\"); }\n";
        let v = two(("src/a.rs", r), ("src/b.rs", r));
        assert!(v.is_empty(), "remove_var: {v:#?}");
        // 15: python-decouple config().
        let c = "url = config('DATABASE_URL_RO')\n";
        let v = two(("app/cache.py", c), ("app/db.py", c));
        assert!(v.is_empty(), "15: {v:#?}");
        // putenv / unsetenv.
        for call in [
            "os.putenv('ORDERS_QUEUE', 'x')",
            "os.unsetenv('ORDERS_QUEUE')",
        ] {
            let src = format!("{call}\n");
            let v = two(("app/a.py", &src), ("app/b.py", &src));
            assert!(v.is_empty(), "{call}: {v:#?}");
        }
        // The control: the same name through an ordinary call is a domain value.
        let v = two(
            ("app/a.py", "register('ORDERS_QUEUE')\n"),
            ("app/b.py", "register('ORDERS_QUEUE')\n"),
        );
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    // ── review fixes: HTML attribute values ────────────────────────────────────

    #[test]
    fn jsx_attribute_values_are_not_counted_cases_01_02_03_10_65_72_88() {
        let cases: [(&str, &str, &str); 7] = [
            (
                "01",
                "export const A = () => <input autoComplete=\"current-password\" />;\n",
                "export const B = () => <input autoComplete=\"current-password\" />;\n",
            ),
            (
                "02",
                "export const A = () => <input type=\"datetime-local\" />;\n",
                "export const B = () => <input type=\"datetime-local\" />;\n",
            ),
            (
                "03",
                "export const A = () => <Icon name=\"chevron-down\" />;\n",
                "export const B = () => <Icon name=\"chevron-down\" />;\n",
            ),
            (
                "10",
                "export const A = () => <Trans i18nKey=\"checkout-title\" />;\n",
                "export const B = () => <Trans i18nKey=\"checkout-title\" />;\n",
            ),
            (
                "65",
                "export const A = () => <iframe sandbox=\"allow-scripts\" />;\n",
                "export const B = () => <iframe sandbox=\"allow-scripts\" />;\n",
            ),
            (
                "72",
                "export const A = () => <button data-testid=\"submit-button\" />;\n",
                "export const B = () => <button data-testid=\"submit-button\" />;\n",
            ),
            (
                "88",
                "export const A = () => <input autoComplete={'current-password'} />;\n",
                "export const B = () => <input {...p} autoComplete={cond ? 'current-password' : 'off'} />;\n",
            ),
        ];
        for (id, a, b) in cases {
            let v = run_pairs(vec![("src/A.tsx", a), ("src/B.tsx", b)]);
            assert!(v.is_empty(), "{id}: {v:#?}");
            // The control: the same text as a plain call argument is flagged.
            let plain = a
                .split('"')
                .nth(1)
                .or_else(|| a.split('\'').nth(1))
                .unwrap()
                .to_string();
            let call = format!("pick('{plain}');\n");
            let v = run_pairs(vec![("src/a.ts", &call), ("src/b.ts", &call)]);
            assert_eq!(v.len(), 1, "{id} control: {v:#?}");
        }
    }

    #[test]
    fn a_route_in_href_or_to_is_still_counted_cases_06_82() {
        let v = run_pairs(vec![
            ("src/Nav.tsx", "export const Nav = () => <Link to=\"/orders/new\">New</Link>;\n"),
            (
                "src/router.tsx",
                "export const routes = createBrowserRouter([{ path: '/orders/new', element: <New/> }]);\n",
            ),
        ]);
        assert_eq!(v.len(), 1, "06: {v:#?}");
        let v = run_pairs(vec![
            (
                "src/components/Empty.tsx",
                "export const E = () => <Link href=\"/orders/new\">Create</Link>;\n",
            ),
            (
                "src/components/Nav.tsx",
                "export const Nav = () => <Link href=\"/orders/new\">New</Link>;\n",
            ),
        ]);
        assert_eq!(v.len(), 1, "82: {v:#?}");
    }

    #[test]
    fn django_widget_attrs_are_html_attributes_case_60() {
        let w = "class F(forms.Form):\n    pw = forms.CharField(widget=forms.PasswordInput(attrs={'autocomplete': 'current-password'}))\n";
        let v = run_pairs(vec![("app/forms.py", w), ("app/forms2.py", w)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn preact_class_attribute_in_a_jsx_file_is_read_cleanly_case_07() {
        let a = "export const A = () => <div class=\"is-active\">a</div>;\n";
        let b = "export const B = () => <div class=\"is-active\">b</div>;\n";
        let v = run_pairs(vec![("src/a.jsx", a), ("src/b.jsx", b)]);
        assert!(v.is_empty(), "{v:#?}");
        let v = run_pairs(vec![("src/a.tsx", a), ("src/b.tsx", b)]);
        assert!(v.is_empty(), "90: {v:#?}");
    }

    // ── review fixes: positions ────────────────────────────────────────────────

    #[test]
    fn a_union_type_member_is_not_a_copy_case_54() {
        let v = run_pairs(vec![
            ("src/a.ts", "fetch('/api/orders');\n"),
            ("src/b.ts", "fetch('/api/orders');\n"),
            (
                "src/types.ts",
                "export type R = '/api/orders' | '/api/users';\n",
            ),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        let m = &v[0].message;
        assert!(m.contains("bare literal in 2 files"), "{m}");
        assert!(!m.contains("types.ts"), "{m}");
    }

    #[test]
    fn class_list_calls_are_class_names_cases_08_71() {
        let a = "export function open(el: HTMLElement) { el.classList.add('is-open'); }\n";
        let b = "export function show(el: HTMLElement) { el.classList.toggle('is-open'); }\n";
        let v = run_pairs(vec![("src/menu.ts", a), ("src/modal.ts", b)]);
        assert!(v.is_empty(), "{v:#?}");
        let v = run_pairs(vec![
            ("src/menu.ts", "el.classList.add('is-active');\n"),
            ("src/modal.ts", "el.classList.toggle('is-active');\n"),
        ]);
        assert!(v.is_empty(), "71: {v:#?}");
    }

    #[test]
    fn string_concatenation_is_not_arithmetic_cases_83_31() {
        let v = run_pairs(vec![
            ("src/a.ts", "export const m = 'rate ' + 0.12;\n"),
            ("src/b.ts", "export const n = 'fee ' + 0.12;\n"),
        ]);
        assert!(v.is_empty(), "83: {v:#?}");
        let v = run_pairs(vec![
            ("src/a.ts", "export const msg = 'rate ' + 0.12;\n"),
            (
                "src/b.ts",
                "export const t = (total: number) => total * 0.12;\n",
            ),
        ]);
        assert!(v.is_empty(), "31: {v:#?}");
        let v = run_pairs(vec![
            ("app/a.py", "m = 'rate ' + 0.12\n"),
            ("app/b.py", "t = total * 0.12\n"),
        ]);
        assert!(v.is_empty(), "python: {v:#?}");
    }

    #[test]
    fn a_comment_before_the_specifier_does_not_hide_it_cases_76_94_57() {
        let v = run_pairs(vec![
            (
                "src/a.js",
                "const m = import(/* webpackChunkName: \"x\" */ 'lodash-es');\n",
            ),
            ("src/b.js", "use('lodash-es');\n"),
            ("src/c.js", "use2('lodash-es');\n"),
        ]);
        assert!(v.is_empty(), "76: {v:#?}");
        let v = run_pairs(vec![
            (
                "app/a.py",
                "import importlib\nm = importlib.import_module(\n    # plugin\n    'plugins-core')\n",
            ),
            ("app/b.py", "use('plugins-core')\n"),
            ("app/c.py", "use2('plugins-core')\n"),
        ]);
        assert!(v.is_empty(), "94: {v:#?}");
    }

    // ── scope decisions ────────────────────────────────────────────────────────

    #[test]
    fn rates_need_two_significant_digits_cases_28_29_30() {
        for (id, a, b) in [
            (
                "28",
                "export const s = (ms: number) => ms * 0.001;\n",
                "export const t = (ms: number) => ms * 0.001;\n",
            ),
            (
                "29",
                "export const w = (x: number) => x * 0.9;\n",
                "export const v = (x: number) => x * 0.9;\n",
            ),
        ] {
            let v = run_pairs(vec![("src/a.ts", a), ("src/b.ts", b)]);
            assert!(v.is_empty(), "{id}: {v:#?}");
        }
        let v = run_pairs(vec![
            ("src/a.py", "def f(x):\n    return x + 1e-9\n"),
            ("src/b.py", "def g(y):\n    return y + 1e-9\n"),
        ]);
        assert!(v.is_empty(), "30: {v:#?}");
        // Two significant digits stay in scope, with any number of leading zeros.
        for rate in ["0.12", "0.075", "0.0825"] {
            let src = format!("export const t = (x: number) => x * {rate};\n");
            let v = run_pairs(vec![("src/a.ts", &src), ("src/b.ts", &src)]);
            assert_eq!(v.len(), 1, "{rate}: {v:#?}");
        }
    }

    #[test]
    fn django_url_names_are_not_domain_values_case_16() {
        let v = run_pairs(vec![
            (
                "orders/views.py",
                "from django.shortcuts import redirect\ndef done(request):\n    return redirect('orders:detail', pk=1)\n",
            ),
            (
                "payments/views.py",
                "from django.urls import reverse\ndef back(request):\n    return reverse('orders:detail', args=[1])\n",
            ),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        for call in ["reverse", "reverse_lazy", "redirect", "resolve_url"] {
            let src = format!("x = {call}('orders:detail')\n");
            let other = "y = pick('orders:detail')\n";
            let v = run_pairs(vec![
                ("app/a.py", &src),
                ("app/b.py", other),
                ("app/c.py", other),
            ]);
            assert!(v.is_empty(), "{call}: {v:#?}");
        }
        // A route passed to redirect() is a route, not a URL name.
        let v = run_pairs(vec![
            ("src/a.ts", "res.redirect('/orders/new');\n"),
            ("src/b.ts", "res.redirect('/orders/new');\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    #[test]
    fn values_group_within_the_nearest_package_root_case_46() {
        let src = "fetch('/api/v1/orders');\n";
        // Conventional monorepo containers.
        let v = run_pairs(vec![
            ("apps/web/src/api.ts", src),
            ("apps/mobile/src/api.ts", src),
        ]);
        assert!(v.is_empty(), "46: {v:#?}");
        // Declared packages.
        let v = run_pairs(vec![
            ("web/package.json", "{}"),
            ("mobile/package.json", "{}"),
            ("web/src/api.ts", src),
            ("mobile/src/api.ts", src),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        let v = run_pairs(vec![
            ("svc/pyproject.toml", ""),
            ("lib/pyproject.toml", ""),
            ("svc/app/a.py", "c.get('/api/v1/orders')\n"),
            ("lib/app/b.py", "c.get('/api/v1/orders')\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        let v = run_pairs(vec![
            ("crates/a/Cargo.toml", ""),
            ("crates/b/Cargo.toml", ""),
            (
                "crates/a/src/lib.rs",
                "fn f() { c.get(\"/api/v1/orders\"); }\n",
            ),
            (
                "crates/b/src/lib.rs",
                "fn g() { c.get(\"/api/v1/orders\"); }\n",
            ),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // Two files inside one package still group.
        let v = run_pairs(vec![
            ("web/package.json", "{}"),
            ("web/src/api.ts", src),
            ("web/src/other/api.ts", src),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
        // And a home in one package does not make another package's copy a bare copy of it.
        let v = run_pairs(vec![
            ("apps/web/src/q.ts", "export const Q = 'orders-high';\n"),
            ("apps/mobile/src/w.ts", "subscribe('orders-high');\n"),
        ]);
        assert!(v.is_empty(), "{v:#?}");
        // No manifest and no container: one package, the repo root.
        let v = run_pairs(vec![("src/a/api.ts", src), ("src/b/api.ts", src)]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    // ── cheap protocol tokens from the review ──────────────────────────────────

    #[test]
    fn cipher_error_code_date_format_region_and_model_tokens_are_protocol_cases_32_34_35_95() {
        for v in [
            "aes-256-gcm",
            "sha-256",
            "rsa-oaep",
            "chacha20-poly1305",
            "hmac-sha256",
            "ERR_MODULE_NOT_FOUND",
            "ERR_INVALID_ARG_TYPE",
            "hh:mm:ss",
            "yyyy-mm-dd",
            "dd/mm/yyyy",
            "us-east-1",
            "eu-central-1",
            "ap-southeast-2",
            "us-gov-west-1",
            "gpt-4o-mini",
            "claude-3-5-sonnet",
            "gemini-1.5-pro",
            "text-embedding-3-small",
        ] {
            let kind = if v.starts_with("ERR_") {
                Shape::ScreamingSnake
            } else {
                Shape::Token
            };
            assert!(
                shape_of(LiteralKind::Str, v).is_some(),
                "{v} must pass the shape first"
            );
            assert!(is_protocol_or_style_value(v), "{v} ({kind:?})");
        }
        for v in [
            "orders-high",
            "retry-queue-2",
            "order:created",
            "us-orders",
            "regions-east-1",
        ] {
            assert!(!is_protocol_or_style_value(v), "{v}");
        }
        let body = "c.createCipheriv('aes-256-gcm', k, iv);\nd.format('hh:mm:ss');\nregion('us-east-1');\nmodel('gpt-4o-mini');\nif (e.code === 'ERR_MODULE_NOT_FOUND') {}\n";
        let v = run_pairs(vec![("src/a.ts", body), ("src/b.ts", body)]);
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn request_and_response_verbs_and_span_callees_take_protocol_names_cases_44_49_61() {
        for call in [
            "req.get('idempotency-key')",
            "res.set('idempotency-key', v)",
            "request.get('idempotency-key')",
            "response.set('idempotency-key', v)",
            "tracer.start_as_current_span('idempotency-key')",
            "tracer.startSpan('idempotency-key')",
            "tracer.startActiveSpan('idempotency-key', f)",
            "Sentry.setTag('idempotency-key', s)",
            "scope.set_tag('idempotency-key', s)",
            "span.setAttribute('idempotency-key', s)",
        ] {
            let src = format!("{call};\n");
            let other = "pick('idempotency-key');\n";
            let v = run_pairs(vec![
                ("src/a.ts", &src),
                ("src/b.ts", other),
                ("src/c.ts", other),
            ]);
            assert!(v.is_empty(), "{call}: {v:#?}");
        }
        // The same verbs on a receiver that is neither a request, a response nor headers.
        let v = run_pairs(vec![
            ("src/a.ts", "store.get('idempotency-key');\n"),
            ("src/b.ts", "cache.set('idempotency-key', v);\n"),
        ]);
        assert_eq!(v.len(), 1, "{v:#?}");
    }

    // ── contract ───────────────────────────────────────────────────────────────

    #[test]
    fn checker_answers_its_rule_id_and_scopes_to_source_and_catalogue_files() {
        assert_eq!(SharedConstantChecker.rule_ids(), &[RULE_SHARED_CONSTANT]);
        for path in [
            "src/a.ts",
            "src/a.tsx",
            "src/a.js",
            "src/a.jsx",
            "src/a.mjs",
            "src/a.cjs",
            "app/a.py",
            "src/a.rs",
            "locales/en.json",
            "src/i18n/en.yaml",
            "public/translations/en.yml",
        ] {
            assert!(
                crate::arch_checker::checker_applies(
                    &SharedConstantChecker,
                    &files(vec![(path, "")])
                ),
                "{path}"
            );
        }
        assert!(!crate::arch_checker::checker_applies(
            &SharedConstantChecker,
            &files(vec![("README.md", ""), ("data/x.json", ""), ("a.go", "")])
        ));
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
        for v in ["0.12", "0.15", "0.999", "0.075", "0.0825", "0.15"] {
            assert!(is_decimal_rate(v), "{v}");
            assert_eq!(shape_of(LiteralKind::Number, v), Some(Shape::Rate), "{v}");
        }
        for v in [
            "0.5",
            "0.25",
            "0.75",
            "0",
            "1",
            "1.5",
            "12",
            "86400",
            "60",
            "1000",
            "404",
            "100",
            "-0.12",
            "NaN",
            "abc",
            "",
            // One significant digit: a scale or an epsilon, not a rate (scope decision A).
            "0.1",
            "0.2",
            "0.9",
            "0.01",
            "0.001",
            "0.000000001",
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
            "app/tests.py",
            "app/test_x.py",
            "orders/test_client.py",
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
