//! Cloud Functions: finding the functions packages and recognising their HTTP and callable
//! handlers (design section 8).
//!
//! Cloud Functions for Firebase written in TypeScript or JavaScript need no new grammar: the
//! existing tree-sitter extractors read them. This module finds the packages, finds the
//! `onRequest` / `onCall` handlers in them, and offers the small textual checks the rules ask of
//! a handler's body (design 7.10). The body checks are textual on purpose: they look for the
//! presence of a call or a reference, never for what a value flows to, and every one is read in
//! the false-negative direction (a token this module does not recognise means "no finding").

use std::collections::HashMap;

use tree_sitter::Node;

use crate::extract::ecma::{self, EcmaDialect};
use crate::extract::paths::is_test_path;
use crate::extract::{imports, lang_for_path, SourceLang};

/// HTTP request handler or callable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandlerKind {
    Request,
    Call,
}

/// One `onRequest` / `onCall` handler.
#[derive(Debug, Clone)]
pub(crate) struct Handler {
    pub kind: HandlerKind,
    /// The exported binding (`createOrder`), or `<anonymous>`.
    pub name: String,
    pub file: String,
    /// 1-based line of the `onRequest` / `onCall` call.
    pub line: usize,
    /// The handler's source (a function expression, a resolved same-file function, or the whole
    /// wrapper call) plus the source of same-file helpers it calls, one level.
    pub body: String,
    /// The formal parameters of the handler function.
    pub params: String,
    /// The options object literal, when the first argument is one.
    pub options: Option<String>,
    /// The callee of the call wrapping the handler (`withAuth`), when it is wrapped.
    pub wrapper: Option<String>,
    /// `onRequest(app)` with `app` bound to an Express or Hono application.
    pub express_app: bool,
}

/// A directory holding a `package.json` that depends on `firebase-functions`.
#[derive(Debug, Clone)]
pub(crate) struct FunctionsPackage {
    pub dir: String,
    /// `(path, content)` of the JS/TS files that belong to it.
    pub files: Vec<(String, String)>,
    pub handlers: Vec<Handler>,
}

/// What discovery found.
#[derive(Debug, Clone, Default)]
pub(crate) struct Discovery {
    pub packages: Vec<FunctionsPackage>,
    /// Directories of Cloud Functions written in Python (not inspected in this version, G13).
    pub python_dirs: Vec<String>,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn in_node_modules(path: &str) -> bool {
    path.split('/').any(|s| s == "node_modules")
}

/// Whether `package.json` text lists `name` in any dependency block.
pub(crate) fn manifest_depends_on(manifest: &str, name: &str) -> bool {
    manifest_dependencies(manifest).iter().any(|d| d == name)
}

/// Every dependency name in `package.json` text (all blocks). Empty when it does not parse.
pub(crate) fn manifest_dependencies(manifest: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(manifest) else {
        return Vec::new();
    };
    [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ]
    .iter()
    .filter_map(|block| value.get(block).and_then(|b| b.as_object()))
    .flat_map(|o| o.keys().cloned())
    .collect()
}

fn is_source_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    !lower.ends_with(".d.ts")
        && ["ts", "tsx", "js", "mjs", "cjs", "mts", "cts"]
            .iter()
            .any(|e| lower.ends_with(&format!(".{e}")))
}

/// Find the functions packages in `files` and the handlers in them.
pub(crate) fn discover(files: &[(String, String)]) -> Discovery {
    let mut dirs: Vec<String> = Vec::new();
    let mut python_dirs: Vec<String> = Vec::new();
    for (path, content) in files {
        if in_node_modules(path) {
            continue;
        }
        match basename(path) {
            "package.json" if manifest_depends_on(content, "firebase-functions") => {
                dirs.push(dir_of(path).to_string());
            }
            "requirements.txt" | "pyproject.toml" if content.contains("firebase-functions") => {
                python_dirs.push(dir_of(path).to_string());
            }
            _ => {}
        }
    }
    dirs.sort();
    dirs.dedup();
    python_dirs.sort();
    python_dirs.dedup();
    let mut packages: Vec<FunctionsPackage> = dirs
        .iter()
        .map(|dir| FunctionsPackage {
            dir: dir.clone(),
            files: Vec::new(),
            handlers: Vec::new(),
        })
        .collect();
    for (path, content) in files {
        if in_node_modules(path) || !is_source_file(path) || is_test_path(path) {
            continue;
        }
        // The nearest enclosing functions package.
        let Some(package) = packages
            .iter_mut()
            .filter(|p| p.dir.is_empty() || path.starts_with(&format!("{}/", p.dir)))
            .max_by_key(|p| p.dir.len())
        else {
            continue;
        };
        let relative = if package.dir.is_empty() {
            path.as_str()
        } else {
            &path[package.dir.len() + 1..]
        };
        // Compiled output is not source.
        if matches!(relative.split('/').next(), Some("lib" | "dist")) {
            continue;
        }
        package.files.push((path.clone(), content.clone()));
    }
    for package in &mut packages {
        let mut handlers = Vec::new();
        for (path, content) in &package.files {
            handlers.extend(handlers_in_file(path, content));
        }
        package.handlers = handlers;
    }
    Discovery {
        packages,
        python_dirs,
    }
}

fn dialect_for(path: &str) -> Option<EcmaDialect> {
    match lang_for_path(path)? {
        SourceLang::TypeScript => Some(EcmaDialect::TypeScript),
        SourceLang::Tsx => Some(EcmaDialect::Tsx),
        SourceLang::JavaScript => Some(EcmaDialect::JavaScript),
        SourceLang::Rust | SourceLang::Python => None,
    }
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// Whether the file reads from the Firebase Functions SDK at all.
fn uses_functions_sdk(path: &str, source: &str) -> bool {
    let Some(lang) = lang_for_path(path) else {
        return false;
    };
    imports(lang, source)
        .iter()
        .any(|i| i.specifier.starts_with("firebase-functions"))
        || source.contains("require('firebase-functions")
        || source.contains("require(\"firebase-functions")
}

/// Every handler in one file.
pub(crate) fn handlers_in_file(path: &str, source: &str) -> Vec<Handler> {
    let Some(dialect) = dialect_for(path) else {
        return Vec::new();
    };
    if !uses_functions_sdk(path, source) {
        return Vec::new();
    }
    let Some(tree) = ecma::parse(dialect, source) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let decls = declarations(root, source);
    let mut out = Vec::new();
    // An explicit stack: a hostile file cannot overflow the call stack.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            if let Some(h) = handler_at(node, path, source, &decls) {
                out.push(h);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out.sort_by_key(|h| h.line);
    out
}

/// Top-level named functions and variable initialisers, by name.
fn declarations<'t>(root: Node<'t>, source: &str) -> HashMap<String, Node<'t>> {
    let mut map = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "function_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    map.insert(text(name, source).to_string(), node);
                }
            }
            "variable_declarator" => {
                if let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) {
                    map.insert(text(name, source).to_string(), value);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    map
}

fn is_function_node(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "function" | "function_declaration"
    )
}

fn handler_kind(callee: Node<'_>, source: &str) -> Option<HandlerKind> {
    let kind_of = |name: &str| match name {
        "onRequest" => Some(HandlerKind::Request),
        "onCall" => Some(HandlerKind::Call),
        _ => None,
    };
    match callee.kind() {
        // `onRequest(...)` / `onCall(...)` imported by name.
        "identifier" => kind_of(text(callee, source)),
        // `https.onRequest(...)`, `functions.https.onCall(...)`,
        // `functions.region('x').https.onRequest(...)`.
        "member_expression" => {
            let property = callee.child_by_field_name("property")?;
            let object = callee.child_by_field_name("object")?;
            let through_https = match object.kind() {
                "identifier" => text(object, source) == "https",
                "member_expression" => object
                    .child_by_field_name("property")
                    .is_some_and(|p| text(p, source) == "https"),
                _ => false,
            };
            if through_https {
                kind_of(text(property, source))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn handler_at(
    call: Node<'_>,
    path: &str,
    source: &str,
    decls: &HashMap<String, Node<'_>>,
) -> Option<Handler> {
    let callee = call.child_by_field_name("function")?;
    let kind = handler_kind(callee, source)?;
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let args: Vec<Node<'_>> = arguments.named_children(&mut cursor).collect();
    let handler_arg = *args.last()?;
    let options =
        (args.len() >= 2 && args[0].kind() == "object").then(|| text(args[0], source).to_string());

    let mut express_app = false;
    let mut wrapper = None;
    let function_node: Option<Node<'_>> = match handler_arg.kind() {
        k if is_function_node(handler_arg) && k != "function_declaration" => Some(handler_arg),
        "identifier" => {
            let name = text(handler_arg, source);
            match decls.get(name).copied() {
                Some(n) if is_function_node(n) => Some(n),
                Some(n) if is_framework_app(n, source) => {
                    express_app = true;
                    None
                }
                _ => return None,
            }
        }
        "call_expression" => {
            wrapper = handler_arg
                .child_by_field_name("function")
                .map(|c| text(c, source).to_string());
            first_function(handler_arg)
        }
        _ => return None,
    };
    let (body, params) = match (function_node, express_app) {
        (Some(f), _) => (
            text(f, source).to_string(),
            f.child_by_field_name("parameters")
                .or_else(|| f.child_by_field_name("parameter"))
                .map(|p| text(p, source).to_string())
                .unwrap_or_default(),
        ),
        (None, true) => (String::new(), String::new()),
        (None, false) => (text(handler_arg, source).to_string(), String::new()),
    };
    let body = with_helpers(&body, decls, source);
    Some(Handler {
        kind,
        name: exported_name(call, source),
        file: path.to_string(),
        line: call.start_position().row + 1,
        body,
        params,
        options,
        wrapper,
        express_app,
    })
}

/// `express()` or `new Hono()`: an application whose routes carry the verification, if any.
fn is_framework_app(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "call_expression" => node
            .child_by_field_name("function")
            .is_some_and(|f| text(f, source) == "express"),
        "new_expression" => node
            .child_by_field_name("constructor")
            .is_some_and(|c| matches!(text(c, source), "Hono" | "express")),
        _ => false,
    }
}

/// The first function expression among the descendants of `node`.
fn first_function(node: Node<'_>) -> Option<Node<'_>> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if is_function_node(n) && n.kind() != "function_declaration" {
            return Some(n);
        }
        let mut cursor = n.walk();
        let mut children: Vec<Node<'_>> = n.children(&mut cursor).collect();
        children.reverse();
        stack.extend(children);
    }
    None
}

/// The exported binding a handler call is assigned to.
fn exported_name(call: Node<'_>, source: &str) -> String {
    let mut current = call.parent();
    let mut depth = 0;
    while let Some(node) = current {
        depth += 1;
        if depth > 8 {
            break;
        }
        match node.kind() {
            "variable_declarator" => {
                if let Some(name) = node.child_by_field_name("name") {
                    return text(name, source).to_string();
                }
            }
            "assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let l = text(left, source);
                    for prefix in ["module.exports.", "exports."] {
                        if let Some(rest) = l.strip_prefix(prefix) {
                            return rest.to_string();
                        }
                    }
                }
            }
            "pair" => {
                if let Some(key) = node.child_by_field_name("key") {
                    return text(key, source).trim_matches(['"', '\'']).to_string();
                }
            }
            "program" => break,
            _ => {}
        }
        current = node.parent();
    }
    "<anonymous>".to_string()
}

/// `body` plus the source of the same-file functions it calls, one level.
fn with_helpers(body: &str, decls: &HashMap<String, Node<'_>>, source: &str) -> String {
    let mut out = body.to_string();
    let mut seen: Vec<&str> = Vec::new();
    for (name, node) in decls {
        if !is_function_node(*node) || seen.contains(&name.as_str()) {
            continue;
        }
        if body.contains(&format!("{name}(")) {
            seen.push(name);
            out.push('\n');
            out.push_str(text(*node, source));
        }
    }
    out
}

// ── textual checks on a handler's body ──────────────────────────────────────────────────────

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// `needle` occurs in `hay` not glued to a preceding identifier character.
fn contains_token(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        hay[..i]
            .chars()
            .last()
            .is_none_or(|c| !is_ident_char(c) && c != '.')
    })
}

/// Server-side verification of a caller: an ID token, a session cookie, an App Check token.
pub(crate) fn verifies_caller(text: &str) -> bool {
    ["verifyIdToken(", "verifySessionCookie(", "verifyToken("]
        .iter()
        .any(|t| text.contains(t))
}

/// Admin SDK receivers a write call can be reached through.
const WRITE_CHAIN_MARKERS: &[&str] = &[
    ".collection(",
    ".doc(",
    ".ref(",
    ".child(",
    ".file(",
    "getFirestore(",
    "firestore()",
    "getDatabase(",
    "database()",
    "getStorage(",
    "bucket(",
];

const WRITE_METHODS: &[&str] = &[
    ".set(", ".update(", ".delete(", ".add(", ".create(", ".push(", ".remove(",
];

/// Whether `text` performs an Admin SDK write: a write method on a Firestore, Realtime Database
/// or Storage chain, a batch or transaction write, a batch commit, or `runTransaction(`.
pub(crate) fn writes_with_admin_sdk(text: &str) -> bool {
    if text.contains("runTransaction(") || text.contains("batch.commit(") {
        return true;
    }
    // Identifiers bound to an Admin SDK chain: `const ref = db.collection('x').doc();`.
    let mut bound: Vec<String> = Vec::new();
    for keyword in ["const ", "let ", "var "] {
        for (at, _) in text.match_indices(keyword) {
            let rest = &text[at + keyword.len()..];
            let name: String = rest.chars().take_while(|c| is_ident_char(*c)).collect();
            if name.is_empty() {
                continue;
            }
            let statement: String = rest.chars().take(300).take_while(|c| *c != ';').collect();
            if WRITE_CHAIN_MARKERS.iter().any(|m| statement.contains(m)) {
                bound.push(name);
            }
        }
    }
    for method in WRITE_METHODS {
        for (at, _) in text.match_indices(method) {
            let before = &text[..at];
            let receiver: String = before
                .chars()
                .rev()
                .take_while(|c| is_ident_char(*c))
                .collect::<Vec<char>>()
                .into_iter()
                .rev()
                .collect();
            if matches!(receiver.as_str(), "batch" | "transaction" | "tx" | "trx")
                || bound.contains(&receiver)
            {
                return true;
            }
            let start = before.char_indices().rev().nth(400).map_or(0, |(i, _)| i);
            let window = &before[start..];
            let window = window.rsplit(';').next().unwrap_or(window);
            if WRITE_CHAIN_MARKERS.iter().any(|m| window.contains(m)) {
                return true;
            }
        }
    }
    false
}

/// A document or collection path built from the caller's input: `.doc(request.data.id)`.
pub(crate) fn uses_caller_supplied_id(text: &str) -> bool {
    [".doc(", ".collection(", ".child(", ".ref("]
        .iter()
        .any(|call| {
            text.match_indices(call).any(|(at, _)| {
                let rest = &text[at + call.len()..];
                let mut depth = 1usize;
                let mut end = rest.len().min(300);
                for (i, c) in rest.char_indices().take(300) {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let args = &rest[..end];
                contains_token(args, "data.") || args.contains("request.data")
            })
        })
}

/// Whether a callable handler reads the caller's identity: `request.auth`, `context.auth`, an
/// `auth` destructured from the request or from the parameter list.
pub(crate) fn references_auth(params: &str, body: &str) -> bool {
    let mut names: Vec<String> = ["request", "context", "req", "ctx"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    names.extend(
        params
            .split(|c: char| !is_ident_char(c))
            .filter(|w| !w.is_empty())
            .map(str::to_string),
    );
    let reads_member = names.iter().any(|n| {
        let needle = format!("{n}.auth");
        body.match_indices(&needle).any(|(i, _)| {
            let before_ok = body[..i]
                .chars()
                .last()
                .is_none_or(|c| !is_ident_char(c) && c != '.');
            let after = body[i + needle.len()..].chars().next();
            before_ok && after.is_none_or(|c| !is_ident_char(c) && c != '(')
        })
    });
    if reads_member {
        return true;
    }
    // `{ auth, data }` in the parameter list.
    if params
        .split(|c: char| !is_ident_char(c))
        .any(|w| w == "auth")
    {
        return true;
    }
    // `const { auth } = request;`
    body.match_indices('}').any(|(i, _)| {
        let after = body[i + 1..].trim_start();
        let Some(rest) = after.strip_prefix('=') else {
            return false;
        };
        let source_name: String = rest
            .trim_start()
            .chars()
            .take_while(|c| is_ident_char(*c))
            .collect();
        if !names.contains(&source_name) {
            return false;
        }
        let open = body[..i].rfind('{').unwrap_or(0);
        body[open..i]
            .split(|c: char| !is_ident_char(c))
            .any(|w| w == "auth")
    })
}

/// A helper whose name says it checks authentication: `requireAuth`, `assertAuthenticated`,
/// `authGuard`.
pub(crate) fn calls_auth_helper(body: &str) -> bool {
    let mut idents: Vec<String> = Vec::new();
    for (at, _) in body.match_indices('(') {
        let name: String = body[..at]
            .chars()
            .rev()
            .take_while(|c| is_ident_char(*c))
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        if !name.is_empty() {
            idents.push(name.to_ascii_lowercase());
        }
    }
    idents.iter().any(|n| auth_helper_name(n))
}

fn auth_helper_name(lower: &str) -> bool {
    let after = |prefixes: &[&str], then: &[&str]| {
        prefixes.iter().any(|p| {
            lower
                .find(p)
                .is_some_and(|i| then.iter().any(|t| lower[i + p.len()..].contains(t)))
        })
    };
    after(&["require", "assert", "ensure", "check"], &["auth"])
        || after(&["auth"], &["require", "guard", "check"])
}

/// A wrapper whose name mentions authentication (`withAuth`, `authenticated`).
pub(crate) fn auth_wrapper(name: &str) -> bool {
    name.to_ascii_lowercase().contains("auth")
}

/// The module handles third-party webhooks: a signature check is the authentication.
pub(crate) fn handles_webhooks(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    [
        "constructevent(",
        "webhooks.",
        "verifysignature",
        "timingsafeequal(",
        "x-hub-signature",
        "x-signature",
        // Twilio: the SDK's signature check and the header it reads.
        "validaterequest",
        "x-twilio-signature",
        "svix",
        "hmac",
    ]
    .iter()
    .any(|t| lower.contains(t))
}

/// Function names and paths that mark a deliberately public endpoint (design 7.10).
pub(crate) fn name_is_exempt(name: &str, path: &str) -> bool {
    const TOKENS: &[&str] = &[
        "webhook",
        "hook",
        "callback",
        "health",
        "ping",
        "sitemap",
        "robots",
        "og",
        "image",
        "thumbnail",
        "cron",
        "scheduled",
    ];
    let haystack = format!("{name} {path}").to_ascii_lowercase();
    TOKENS.iter().any(|t| haystack.contains(t))
}

/// An `invoker` option that is not `"public"`: IAM already limits who may call the function.
pub(crate) fn invoker_is_restricted(options: Option<&str>) -> bool {
    let Some(options) = options else {
        return false;
    };
    let Some(at) = options.find("invoker") else {
        return false;
    };
    let rest = options[at + "invoker".len()..].trim_start();
    let Some(value) = rest.strip_prefix(':') else {
        return false;
    };
    let value = value.trim_start();
    match value.chars().next() {
        Some('"' | '\'') => {
            let inner: String = value[1..]
                .chars()
                .take_while(|c| *c != '"' && *c != '\'')
                .collect();
            inner != "public"
        }
        Some('[') => true,
        _ => false,
    }
}
