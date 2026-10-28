//! Python tests (pytest, unittest): in a test module, a module-level `def test*` and a `test*`
//! method of a class named `Test*` or deriving from a `*TestCase` class.
//!
//! A test is skipped, never reported, when it or its class carries a decorator outside a short
//! list known not to assert (marks other than skip/skipif/xfail, `patch`, `given`, `settings`,
//! `freeze_time`, `override_settings`, ...), when a skip decorator applies, and when it requests
//! a fixture that might assert (defined in the module or a `conftest.py` above it, directly or
//! through a fixture it requests) or whose name says it checks (`snapshot`, `assert_*`). A module
//! wrapped by an `autouse` fixture that might assert is not read at all.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use super::words::{is_support_module, message, names_an_assertion, HELPER_DEPTH};
use super::Finding;
use crate::extract::ecma_walk::{descendants, line, named_children, text};
use crate::extract::python::parse;
use crate::extract::resolver::directory_of;

/// Decorator names (the last dotted segment, call arguments ignored) known not to assert.
const INERT_DECORATORS: &[&str] = &[
    "parametrize",
    "asyncio",
    "anyio",
    "django_db",
    "usefixtures",
    "timeout",
    "slow",
    "flaky",
    "patch",
    "object",
    "dict",
    "multiple",
    "given",
    "settings",
    "example",
    "freeze_time",
    "override_settings",
    "modify_settings",
    "tag",
    "skip_if_no_network",
];

/// Decorator names that mean the test does not run.
const SKIP_DECORATORS: &[&str] = &[
    "skip",
    "skipif",
    "skipIf",
    "skipUnless",
    "xfail",
    "expectedFailure",
];

/// A module the test runners collect: pytest's `test_*.py` and `*_test.py`, and unittest's and
/// Django's `test*.py` (which covers `tests.py`). A helper module in a test folder is not one.
pub(super) fn is_test_module(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = file.strip_suffix(".py") else {
        return false;
    };
    stem.starts_with("test") || stem.ends_with("_test")
}

/// What the fixtures a test module can request might check: pytest runs a fixture's code before
/// and after every test that requests it (and every test, for an `autouse` fixture), so an
/// assertion there covers the test.
pub(super) struct Fixtures {
    /// An `autouse` fixture that might assert wraps every test of the module.
    pub(super) autouse_asserts: bool,
    /// Fixtures that might assert, directly or through a fixture they request.
    asserting: HashSet<String>,
}

/// The fixtures defined in the module and in every `conftest.py` in its folder or above it.
pub(super) fn visible_fixtures(path: &str, source: &str, files: &[(String, String)]) -> Fixtures {
    let dir = directory_of(path);
    let conftests = files.iter().filter_map(|(p, c)| {
        let conftest_dir = p.strip_suffix("conftest.py")?.trim_end_matches('/');
        let above = conftest_dir.is_empty()
            || dir == conftest_dir
            || dir.starts_with(&format!("{conftest_dir}/"));
        above.then_some(c.as_str())
    });
    let mut defined: Vec<FixtureDef> = Vec::new();
    let mut unreadable = false;
    for text in std::iter::once(source).chain(conftests) {
        if !text.contains("fixture") {
            continue;
        }
        match fixture_defs(text) {
            Some(defs) => defined.extend(defs),
            None => unreadable = true,
        }
    }
    let mut asserting: HashSet<String> = defined
        .iter()
        .filter(|f| f.asserts)
        .map(|f| f.name.clone())
        .collect();
    loop {
        let before = asserting.len();
        for f in &defined {
            if f.requests.iter().any(|r| asserting.contains(r)) {
                asserting.insert(f.name.clone());
            }
        }
        if asserting.len() == before {
            break;
        }
    }
    Fixtures {
        autouse_asserts: unreadable
            || defined
                .iter()
                .any(|f| f.autouse && asserting.contains(&f.name)),
        asserting,
    }
}

/// One fixture definition.
struct FixtureDef {
    name: String,
    autouse: bool,
    /// Its own body holds something that might assert.
    asserts: bool,
    /// The fixtures it requests (its parameters).
    requests: Vec<String>,
}

/// Every function decorated as a fixture, or `None` when the file cannot be parsed.
fn fixture_defs(source: &str) -> Option<Vec<FixtureDef>> {
    let tree = parse(source)?;
    let root = tree.root_node();
    let module = Module::read(root, source);
    let defs = descendants(root)
        .into_iter()
        .filter(|n| n.kind() == "decorated_definition")
        .filter_map(|n| {
            let (definition, decorators) = unwrap_decorated(n);
            let fixture = decorators
                .iter()
                .find(|d| decorator_name(d, source) == "fixture")?;
            let name = text(definition.child_by_field_name("name")?, source).to_string();
            let asserts = definition
                .child_by_field_name("body")
                .is_none_or(|b| module.asserts(b, None, HELPER_DEPTH));
            Some(FixtureDef {
                name,
                autouse: text(*fixture, source).contains("autouse"),
                asserts,
                requests: parameter_names(definition, source)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect();
    Some(defs)
}

/// The names of a function's parameters.
fn parameter_names<'s>(function: Node<'_>, source: &'s str) -> Vec<&'s str> {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return Vec::new();
    };
    named_children(parameters)
        .into_iter()
        .filter_map(|p| match p.kind() {
            "identifier" => Some(text(p, source)),
            _ => p
                .child_by_field_name("name")
                .or_else(|| named_children(p).into_iter().next())
                .filter(|n| n.kind() == "identifier")
                .map(|n| text(n, source)),
        })
        .collect()
}

pub(super) fn check(source: &str, fixtures: &Fixtures) -> Vec<Finding> {
    let Some(tree) = parse(source) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut module = Module::read(root, source);
    module.asserting_fixtures = Some(&fixtures.asserting);
    let mut out = Vec::new();
    for statement in named_children(root) {
        let (definition, decorators) = unwrap_decorated(statement);
        match definition.kind() {
            "function_definition" => {
                if let Some(f) = module.test_without_assertion(definition, &decorators, None) {
                    out.push(f);
                }
            }
            "class_definition" if is_test_class(definition, source) => {
                if decorators.iter().any(|d| !is_inert(d, source)) {
                    continue;
                }
                let Some(body) = definition.child_by_field_name("body") else {
                    continue;
                };
                let methods: HashMap<&str, Node<'_>> = named_children(body)
                    .into_iter()
                    .map(|m| unwrap_decorated(m).0)
                    .filter(|m| m.kind() == "function_definition")
                    .filter_map(|m| Some((text(m.child_by_field_name("name")?, source), m)))
                    .collect();
                for member in named_children(body) {
                    let (method, decorators) = unwrap_decorated(member);
                    if method.kind() != "function_definition" {
                        continue;
                    }
                    if let Some(f) =
                        module.test_without_assertion(method, &decorators, Some(&methods))
                    {
                        out.push(f);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A definition and the decorators wrapping it.
fn unwrap_decorated(node: Node<'_>) -> (Node<'_>, Vec<Node<'_>>) {
    if node.kind() != "decorated_definition" {
        return (node, Vec::new());
    }
    let decorators = named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "decorator")
        .collect();
    let definition = node.child_by_field_name("definition").unwrap_or(node);
    (definition, decorators)
}

/// `class TestX:` or `class X(SomethingTestCase):`.
fn is_test_class(class: Node<'_>, source: &str) -> bool {
    let named_test = class
        .child_by_field_name("name")
        .is_some_and(|n| text(n, source).starts_with("Test"));
    let derives_test_case = class.child_by_field_name("superclasses").is_some_and(|s| {
        named_children(s)
            .iter()
            .any(|b| text(*b, source).ends_with("TestCase"))
    });
    named_test || derives_test_case
}

/// The decorator's name: the last dotted segment of its expression, call arguments left out.
fn decorator_name<'s>(decorator: &Node<'_>, source: &'s str) -> &'s str {
    let expression = named_children(*decorator).into_iter().next();
    let target = expression.map(|e| {
        if e.kind() == "call" {
            e.child_by_field_name("function").unwrap_or(e)
        } else {
            e
        }
    });
    let full = target.map_or("", |t| text(t, source));
    full.rsplit('.').next().unwrap_or(full)
}

/// Known not to assert: a pytest mark (a label) other than a skip, or a listed decorator.
fn is_inert(decorator: &Node<'_>, source: &str) -> bool {
    let full = named_children(*decorator)
        .into_iter()
        .next()
        .map_or("", |e| text(e, source));
    let is_mark = full.starts_with("pytest.mark.") || full.starts_with("mark.");
    is_mark || INERT_DECORATORS.contains(&decorator_name(decorator, source))
}

/// What the module binds: its imports and its function definitions.
struct Module<'t, 's> {
    source: &'s str,
    /// Fixtures that might assert, when reading a test module (`None` inside a fixture file).
    asserting_fixtures: Option<&'s HashSet<String>>,
    /// Local name → module path (`.helpers`, `tests.utils`).
    imports: HashMap<String, String>,
    /// Module-level functions by name (several when a name is defined more than once).
    functions: HashMap<&'s str, Vec<Node<'t>>>,
}

impl<'t, 's> Module<'t, 's> {
    fn read(root: Node<'t>, source: &'s str) -> Self {
        let mut imports = HashMap::new();
        for statement in named_children(root) {
            match statement.kind() {
                "import_from_statement" => {
                    let module = statement
                        .child_by_field_name("module_name")
                        .map_or("", |m| text(m, source));
                    let mut cursor = statement.walk();
                    for name in statement.children_by_field_name("name", &mut cursor) {
                        imports.insert(local_name(name, source), module.to_string());
                    }
                }
                "import_statement" => {
                    let mut cursor = statement.walk();
                    for name in statement.children_by_field_name("name", &mut cursor) {
                        let module = if name.kind() == "aliased_import" {
                            name.child_by_field_name("name")
                                .map_or("", |n| text(n, source))
                        } else {
                            text(name, source)
                        };
                        let local = local_name(name, source);
                        let local = local.split('.').next().unwrap_or_default().to_string();
                        imports.insert(local, module.to_string());
                    }
                }
                _ => {}
            }
        }
        let mut functions: HashMap<&'s str, Vec<Node<'t>>> = HashMap::new();
        for f in named_children(root)
            .into_iter()
            .map(|s| unwrap_decorated(s).0)
        {
            if f.kind() != "function_definition" {
                continue;
            }
            if let Some(name) = f.child_by_field_name("name") {
                functions.entry(text(name, source)).or_default().push(f);
            }
        }
        Self {
            source,
            asserting_fixtures: None,
            imports,
            functions,
        }
    }

    fn test_without_assertion(
        &self,
        function: Node<'_>,
        decorators: &[Node<'_>],
        methods: Option<&HashMap<&str, Node<'_>>>,
    ) -> Option<Finding> {
        let name = text(function.child_by_field_name("name")?, self.source);
        if !name.starts_with("test")
            || decorators.iter().any(|d| {
                SKIP_DECORATORS.contains(&decorator_name(d, self.source))
                    || !is_inert(d, self.source)
            })
            || self.has_checking_fixture(function)
        {
            return None;
        }
        let body = function.child_by_field_name("body")?;
        if self.asserts(body, methods, HELPER_DEPTH) {
            return None;
        }
        let empty = named_children(body).iter().all(|s| is_filler(*s));
        let subject = format!("The test `{name}` (line {})", line(function));
        Some(Finding {
            line: line(function),
            name: name.to_string(),
            message: message(&subject, empty),
        })
    }

    /// A parameter whose name says it checks (`snapshot`, `assert_num_queries`), or a fixture
    /// that might assert.
    fn has_checking_fixture(&self, function: Node<'_>) -> bool {
        parameter_names(function, self.source)
            .into_iter()
            .any(|name| {
                names_an_assertion(name)
                    || self
                        .asserting_fixtures
                        .is_some_and(|fixtures| fixtures.contains(name))
            })
    }

    fn asserts(
        &self,
        node: Node<'_>,
        methods: Option<&HashMap<&str, Node<'_>>>,
        depth: usize,
    ) -> bool {
        descendants(node).into_iter().any(|n| match n.kind() {
            "assert_statement" | "raise_statement" => true,
            "call" => self.call_asserts(n, methods, depth),
            _ => false,
        })
    }

    fn call_asserts(
        &self,
        call: Node<'_>,
        methods: Option<&HashMap<&str, Node<'_>>>,
        depth: usize,
    ) -> bool {
        let Some(callee) = call.child_by_field_name("function") else {
            return false;
        };
        let full = text(callee, self.source);
        let last = full.rsplit('.').next().unwrap_or(full);
        if names_an_assertion(last) {
            return true;
        }
        let root = full.split('.').next().unwrap_or(full);
        if root == "self" || root == "cls" {
            // A method of the test class: followed when defined there, otherwise it lives in a
            // base class the checker cannot see, which may assert.
            let defined = methods.and_then(|m| m.get(last));
            return match defined {
                Some(method) if depth > 0 && full.matches('.').count() == 1 => method
                    .child_by_field_name("body")
                    .is_some_and(|b| self.asserts(b, methods, depth - 1)),
                Some(_) => false,
                None => full.matches('.').count() == 1,
            };
        }
        if let Some(module) = self.imports.get(root) {
            // The test frameworks themselves are not helper modules: `mock.patch(...)` sets up,
            // and their asserting calls (`pytest.raises`) are caught by name above.
            let framework = ["unittest", "pytest"]
                .iter()
                .any(|f| module == f || module.starts_with(&format!("{f}.")));
            return !framework && (module.starts_with('.') || is_support_module(module));
        }
        if callee.kind() == "identifier" && depth > 0 {
            if let Some(helpers) = self.functions.get(root) {
                return helpers.iter().any(|helper| {
                    helper
                        .child_by_field_name("body")
                        .is_some_and(|b| self.asserts(b, methods, depth - 1))
                });
            }
        }
        false
    }
}

/// `x` for `x` or `y as x`.
fn local_name(name: Node<'_>, source: &str) -> String {
    let local = if name.kind() == "aliased_import" {
        name.child_by_field_name("alias").unwrap_or(name)
    } else {
        name
    };
    text(local, source).to_string()
}

/// `pass`, `...` or a docstring.
fn is_filler(statement: Node<'_>) -> bool {
    match statement.kind() {
        "pass_statement" => true,
        "expression_statement" => named_children(statement)
            .iter()
            .all(|e| matches!(e.kind(), "string" | "ellipsis")),
        _ => false,
    }
}
