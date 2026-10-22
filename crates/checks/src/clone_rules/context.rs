//! The per-run lookup state the three rules share: the scan's files by path and a small cache of
//! tokenized files, so a file that belongs to many families is tokenized once, not once per
//! family. The cache is bounded and cleared wholesale when full; that affects time, never output.

use std::collections::HashMap;
use std::rc::Rc;

use crate::extract::clone::{
    token_id, tokens, CodeSpan, ItemTokens, Token, TokenClass, TokenMode, TEXT_ARITH, TEXT_CMP,
    TEXT_LOGIC,
};
use crate::extract::ecma::{self, EcmaDialect};
use crate::extract::{functions, lang_for_path, FunctionSpan, SourceLang};

use super::bindings::{import_bindings, Binding};

/// How many tokenized files each cache holds before it is cleared.
const CACHE_FILES: usize = 384;

/// The Structural token ids a rule asks about. A Structural token carries only an id and a
/// class; to ask "is this a comparison" the rule compares against the id the detector would have
/// computed for it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Ids {
    pub arith: u32,
    pub cmp: u32,
    pub logic: u32,
}

impl Ids {
    pub(super) fn new() -> Self {
        Self {
            arith: token_id(TokenClass::BinOp, TEXT_ARITH),
            cmp: token_id(TokenClass::BinOp, TEXT_CMP),
            logic: token_id(TokenClass::BinOp, TEXT_LOGIC),
        }
    }
}

/// Whether a `package.json` text declares a React-family dependency of any kind.
fn depends_on_react_family(manifest: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(manifest) else {
        return false;
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .any(|section| {
            value
                .get(section)
                .and_then(serde_json::Value::as_object)
                .is_some_and(|deps| REACT_FAMILY.iter().any(|name| deps.contains_key(*name)))
        })
}

/// Whether `token` is the keyword `word`.
pub(super) fn is_keyword(token: &Token, word: &str) -> bool {
    token.class == TokenClass::Keyword && token.id == token_id(TokenClass::Keyword, word)
}

/// Whether `token` is the punctuation `text`.
pub(super) fn is_punct(token: &Token, text: &str) -> bool {
    token.class == TokenClass::Punct && token.id == token_id(TokenClass::Punct, text)
}

/// A span resolved to its tokens. Holds the tokenized file alive, so the cache may be cleared
/// while a view is in use.
pub(super) struct SpanView {
    items: Rc<Vec<ItemTokens>>,
    item: usize,
    start: usize,
    len: usize,
}

impl SpanView {
    pub(super) fn item(&self) -> &ItemTokens {
        &self.items[self.item]
    }

    pub(super) fn tokens(&self) -> &[Token] {
        let all = &self.items[self.item].tokens;
        all.get(self.start..self.start + self.len).unwrap_or(&[])
    }

    /// The Exact text of the span's token at `offset` (an Exact-mode view only).
    pub(super) fn text(&self, offset: usize) -> Option<&str> {
        self.items[self.item]
            .texts
            .get(self.start + offset)
            .map(String::as_str)
    }
}

pub(super) struct Ctx<'a> {
    pub files: &'a [(String, String)],
    pub ids: Ids,
    /// UI-SHARED-PRIMITIVE-1 is armed in this run, so a function that renders JSX in a package
    /// that rule reads belongs to it, not to ARCH-NAMING-ONE-CONCEPT-1.
    pub ui_rule_armed: bool,
    by_path: HashMap<&'a str, usize>,
    structural: HashMap<usize, Rc<Vec<ItemTokens>>>,
    exact: HashMap<usize, Rc<Vec<ItemTokens>>>,
    bindings: HashMap<usize, Rc<Vec<Binding>>>,
    functions: HashMap<usize, Rc<Vec<FunctionSpan>>>,
    trees: HashMap<usize, Rc<tree_sitter::Tree>>,
    react_dirs: HashMap<String, bool>,
}

/// The dependency names that mark a package as React-family UI.
const REACT_FAMILY: &[&str] = &["react", "preact", "next", "react-native", "expo"];

impl<'a> Ctx<'a> {
    pub(super) fn new(files: &'a [(String, String)]) -> Self {
        // The first of any duplicated path wins, matching the order the scan hands files over.
        let mut by_path = HashMap::with_capacity(files.len());
        for (i, (path, _)) in files.iter().enumerate() {
            by_path.entry(path.as_str()).or_insert(i);
        }
        Self {
            files,
            ids: Ids::new(),
            ui_rule_armed: false,
            by_path,
            structural: HashMap::new(),
            exact: HashMap::new(),
            bindings: HashMap::new(),
            functions: HashMap::new(),
            trees: HashMap::new(),
            react_dirs: HashMap::new(),
        }
    }

    fn index_of(&self, path: &str) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    pub(super) fn source(&self, path: &str) -> Option<&'a str> {
        self.index_of(path).map(|i| self.files[i].1.as_str())
    }

    pub(super) fn lang(&self, path: &str) -> Option<SourceLang> {
        lang_for_path(path)
    }

    fn trim(&mut self) {
        if self.structural.len() > CACHE_FILES {
            self.structural.clear();
        }
        if self.exact.len() > CACHE_FILES {
            self.exact.clear();
        }
        if self.bindings.len() > CACHE_FILES {
            self.bindings.clear();
        }
        if self.functions.len() > CACHE_FILES {
            self.functions.clear();
        }
        if self.trees.len() > CACHE_FILES {
            self.trees.clear();
        }
    }

    /// The syntax tree of an ECMAScript file. A JavaScript file the JavaScript grammar cannot
    /// parse cleanly is re-parsed with the TSX grammar, as the tokenizer does, so both read the
    /// same structure.
    pub(super) fn tree(&mut self, path: &str) -> Option<Rc<tree_sitter::Tree>> {
        let index = self.index_of(path)?;
        self.trim();
        if let Some(hit) = self.trees.get(&index) {
            return Some(Rc::clone(hit));
        }
        let source = &self.files[index].1;
        let dialect = match lang_for_path(path)? {
            SourceLang::TypeScript => EcmaDialect::TypeScript,
            SourceLang::Tsx => EcmaDialect::Tsx,
            SourceLang::JavaScript => EcmaDialect::JavaScript,
            SourceLang::Python | SourceLang::Rust => return None,
        };
        let mut tree = ecma::parse(dialect, source)?;
        if dialect == EcmaDialect::JavaScript && tree.root_node().has_error() {
            if let Some(retry) = ecma::parse(EcmaDialect::Tsx, source) {
                tree = retry;
            }
        }
        let tree = Rc::new(tree);
        self.trees.insert(index, Rc::clone(&tree));
        Some(tree)
    }

    /// Whether the nearest `package.json` above `path` depends on a React-family package
    /// (`react`, `preact`, `next`, `react-native`, `expo`). A file with no manifest above it is
    /// out of scope: the rule would be guessing.
    pub(super) fn in_react_package(&mut self, path: &str) -> bool {
        let mut dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        loop {
            if let Some(known) = self.react_dirs.get(&dir) {
                return *known;
            }
            let manifest = if dir.is_empty() {
                "package.json".to_string()
            } else {
                format!("{dir}/package.json")
            };
            if let Some(text) = self.source(&manifest) {
                let verdict = depends_on_react_family(text);
                self.react_dirs.insert(dir, verdict);
                return verdict;
            }
            match dir.rsplit_once('/') {
                Some((parent, _)) => dir = parent.to_string(),
                None if !dir.is_empty() => dir = String::new(),
                None => return false,
            }
        }
    }

    fn tokenized(&mut self, path: &str, mode: TokenMode) -> Option<Rc<Vec<ItemTokens>>> {
        let index = self.index_of(path)?;
        self.trim();
        let cache = match mode {
            TokenMode::Structural => &mut self.structural,
            TokenMode::Exact => &mut self.exact,
        };
        if let Some(hit) = cache.get(&index) {
            return Some(Rc::clone(hit));
        }
        let lang = lang_for_path(path)?;
        let items = Rc::new(tokens(lang, &self.files[index].1, mode));
        cache.insert(index, Rc::clone(&items));
        Some(items)
    }

    /// The span's Structural tokens, or `None` when the file or the span cannot be resolved.
    pub(super) fn structural_view(&mut self, span: &CodeSpan) -> Option<SpanView> {
        self.view(span, TokenMode::Structural)
    }

    /// The span's Exact tokens (with their texts).
    pub(super) fn exact_view(&mut self, span: &CodeSpan) -> Option<SpanView> {
        self.view(span, TokenMode::Exact)
    }

    fn view(&mut self, span: &CodeSpan, mode: TokenMode) -> Option<SpanView> {
        let items = self.tokenized(&span.file, mode)?;
        let item = span.item as usize;
        let start = span.token_start as usize;
        let len = span.token_len as usize;
        let available = items.get(item)?.tokens.len();
        if start.checked_add(len)? > available {
            return None;
        }
        Some(SpanView {
            items,
            item,
            start,
            len,
        })
    }

    /// The import bindings of a file (local name and module specifier).
    pub(super) fn bindings(&mut self, path: &str) -> Rc<Vec<Binding>> {
        let Some(index) = self.index_of(path) else {
            return Rc::new(Vec::new());
        };
        self.trim();
        if let Some(hit) = self.bindings.get(&index) {
            return Rc::clone(hit);
        }
        let found = lang_for_path(path)
            .map(|lang| import_bindings(lang, &self.files[index].1))
            .unwrap_or_default();
        let found = Rc::new(found);
        self.bindings.insert(index, Rc::clone(&found));
        found
    }

    /// The function spans of a file (`extract::functions`).
    pub(super) fn functions(&mut self, path: &str) -> Rc<Vec<FunctionSpan>> {
        let Some(index) = self.index_of(path) else {
            return Rc::new(Vec::new());
        };
        self.trim();
        if let Some(hit) = self.functions.get(&index) {
            return Rc::clone(hit);
        }
        let found = lang_for_path(path)
            .map(|lang| functions(lang, &self.files[index].1))
            .unwrap_or_default();
        let found = Rc::new(found);
        self.functions.insert(index, Rc::clone(&found));
        found
    }

    /// The source text of one token.
    pub(super) fn token_text(&self, path: &str, token: &Token) -> Option<&'a str> {
        let source = self.source(path)?;
        source.get(token.raw.start as usize..token.raw.end as usize)
    }
}
