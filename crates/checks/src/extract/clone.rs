//! The shared token clone detector (W29 Wave C): one candidate producer for
//! ARCH-SINGLE-SOURCE-LOGIC-1, ARCH-NAMING-ONE-CONCEPT-1 and UI-SHARED-PRIMITIVE-1.
//!
//! Spec: `docs/design/2026-10-21_w29-detector-implementation.md`, section "Clone detector:
//! the algorithm and its traps" (and its "Build notes" subsection for every place the build
//! decided something the spec left open).
//!
//! The detector never produces a finding. [`detect`] returns a [`CloneIndex`] of clone
//! FAMILIES: sets of spans, in at least two files of one language family, whose
//! identifier-insensitive ([`TokenMode::Structural`]) token sequences are identical. The three
//! rules read that index; the only sentences that may reach a client are facts provable by token
//! comparison ([`exact_diff`]).
//!
//! # Pipeline
//!
//! 1. **Skip** files that are not source, are tests, generated, vendored, minified, i18n or
//!    data catalogs, migrations or seeds, too large, or do not parse ([`skip`]).
//! 2. **Tokenize** per dialect ([`ts_tokens`] for TypeScript/TSX/JavaScript/Python,
//!    [`rust_tokens`] for Rust) into one [`ItemTokens`] per top-level item. Imports, exports of
//!    other modules, type declarations and comments contribute no tokens; windows never cross
//!    an item.
//! 3. **Fingerprint** (phase 1): rolling hash of every K-gram, winnowed, 16 bytes per kept
//!    fingerprint; the tokens are discarded ([`index`]).
//! 4. **Bucket** (phase 2), **verify and extend** (phase 3), **form families and drop
//!    subsumed ones** (phase 4) ([`index`]).
//!
//! The module is pure over `(path, content)` pairs: no I/O, no clock, no randomness, no model,
//! and it never panics (a file that fails to parse is skipped with [`SkipReason::ParseFailed`]).

use std::ops::Range;

use super::SourceLang;

mod builder;
mod cache;
mod families;
mod index;
mod rust_tokens;
mod skip;
mod ts_tokens;
mod verify;

#[cfg(test)]
mod tests;

pub use index::detect;

/// Hash unit: the number of consecutive tokens one fingerprint covers (spec section 3).
pub const K: usize = 20;

/// Text of the class tokens a Structural stream collapses to. A rule asking "does the shared
/// sequence contain a comparison" compares each token's `id` with
/// `token_id(TokenClass::BinOp, TEXT_CMP)`.
pub const TEXT_IDENT: &str = "$ID";
/// Collapsed property, field, attribute or method name after `.` or `?.`.
pub const TEXT_PROP: &str = "$PROP";
/// Collapsed string literal.
pub const TEXT_STR: &str = "$STR";
/// Collapsed number literal.
pub const TEXT_NUM: &str = "$NUM";
/// Collapsed `true false null undefined None True False`.
pub const TEXT_LIT: &str = "$LIT";
/// Collapsed arithmetic operator family (`+ - * / % **`).
pub const TEXT_ARITH: &str = "$ARITH";
/// Collapsed comparison operator family (`== === != !== < > <= >= in is instanceof`).
pub const TEXT_CMP: &str = "$CMP";
/// Collapsed logical operator family (`&& || ?? and or`).
pub const TEXT_LOGIC: &str = "$LOGIC";
/// Collapsed bitwise operator family (`| & ^ << >> >>>`).
pub const TEXT_BIT: &str = "$BIT";
/// Collapsed unary operator (`! - + ~ typeof void`; `await` and `not` stay verbatim).
pub const TEXT_UNOP: &str = "$UNOP";
/// Collapsed assignment operator family (`= += -= ...`).
pub const TEXT_ASSIGN: &str = "$ASSIGN";

/// How a source token is normalized before hashing. See the table in spec section 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenMode {
    /// Identifier-insensitive: free identifiers, property names, literals and operator
    /// families are each collapsed to a class token, so `title ?? slug` and `name || id`
    /// hash the same. The mode the FAMILY search runs in.
    Structural,
    /// Free identifiers still collapse; property names, literal values and operator text are
    /// kept verbatim. Never used to find families; used to decide whether the members of a
    /// found family AGREE (ARCH-SINGLE-SOURCE-LOGIC-1).
    Exact,
}

/// The syntactic class of one token. A `Token`'s `id` is derived from its class and its
/// normalized text, so two tokens are equal only when both match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenClass {
    /// A free identifier (variable, parameter, function, class or type name, callee).
    Ident,
    /// A property, field, attribute or method name after `.` or `?.`; also an object key.
    Prop,
    /// A JSX element tag name (kept verbatim in both modes).
    JsxTag,
    /// A JSX attribute name (kept verbatim in both modes).
    JsxAttr,
    /// A string or template literal, JSX text, or a regex literal.
    Str,
    /// A number literal.
    Num,
    /// `true false null undefined None True False`.
    Lit,
    /// A binary operator.
    BinOp,
    /// A unary operator.
    UnOp,
    /// An assignment operator.
    Assign,
    /// A keyword, or `this`/`self`/`super`/`cls`.
    Keyword,
    /// Punctuation.
    Punct,
    /// A synthetic Python block brace (`{` on entering a block, `}` on leaving it).
    Block,
}

/// One normalized source token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// A 32-bit FNV-1a hash of the token's class and NORMALIZED text (per mode). Content
    /// derived, so it is identical across runs, files and processes: a family `key` is stable.
    /// Compute the id of a known normalized text with [`token_id`].
    pub id: u32,
    pub class: TokenClass,
    /// 1-based line of the source token.
    pub line: u32,
    /// Byte range in the source, for Exact-mode diffs and messages.
    pub raw: Range<u32>,
}

/// One top-level item's token sequence. Windows never cross an item boundary.
///
/// `item` is the item's index in the `Vec` that [`tokens`] returned, and is identical in both
/// modes (the two modes tokenize the same stream; only the normalization differs), so a
/// [`CodeSpan`] from the index addresses the same tokens in a Structural or an Exact tokenization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemTokens {
    pub item: u32,
    /// What the item is: `function`, `class`, `variable`, `statement`, `namespace`, `data` (a
    /// lookup table), `impl`, `impl-empty`, `trait`, `const`, `static`, `macro` or `other`. A
    /// rule that needs a function body reads this, not the name.
    pub kind: &'static str,
    pub name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub tokens: Vec<Token>,
    /// The normalized text of each token, parallel to `tokens`. Populated ONLY by an
    /// [`TokenMode::Exact`] tokenization (it is the text a [`Disagreement`] names); empty in
    /// Structural mode, where only the ids are needed and the strings would be pure memory cost.
    pub texts: Vec<String>,
}

/// Tokenize `source` per dialect, one [`ItemTokens`] per top-level item that has tokens.
///
/// Never panics. A source that does not parse, or is larger than a `u32` can address, yields an
/// empty vec. Items that contain a parse-error node are dropped rather than guessed at.
pub fn tokens(lang: SourceLang, source: &str, mode: TokenMode) -> Vec<ItemTokens> {
    tokenize(lang, source, mode).unwrap_or_default()
}

/// The token stream of a source file, or `None` when it cannot be tokenized (the detector
/// reports that as [`SkipReason::ParseFailed`]).
pub(crate) fn tokenize(lang: SourceLang, source: &str, mode: TokenMode) -> Option<Vec<ItemTokens>> {
    if u32::try_from(source.len()).is_err() {
        return None;
    }
    match lang {
        SourceLang::Rust => rust_tokens::tokenize(source, mode),
        SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript | SourceLang::Python => {
            ts_tokens::tokenize(lang, source, mode)
        }
    }
}

/// The id of a token with this class and NORMALIZED text, as [`Token::id`] holds it.
/// `token_id(TokenClass::Ident, TEXT_IDENT)` is the id of every Structural identifier;
/// `token_id(TokenClass::Keyword, "if")` the id of an `if`.
pub fn token_id(class: TokenClass, text: &str) -> u32 {
    const OFFSET: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    let mut hash = (OFFSET ^ u32::from(class as u8)).wrapping_mul(PRIME);
    for byte in text.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(PRIME);
    }
    hash
}

/// One occurrence of a clone: `token_len` tokens of item `item` of `file`, starting at token
/// `token_start`. Lines are those of the span's first and last token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodeSpan {
    pub file: String,
    pub item: u32,
    pub token_start: u32,
    pub token_len: u32,
    pub start_line: u32,
    pub end_line: u32,
}

/// A set of spans whose Structural token sequences are identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneFamily {
    /// FNV-1a (64-bit) of the shared Structural token ids; stable across runs.
    pub key: u64,
    /// Length of the shared sequence (every member has exactly it).
    pub token_len: u32,
    /// Sorted by `(file, start_line)`; at least two distinct files.
    pub members: Vec<CodeSpan>,
    pub distinct_files: u32,
    /// The family is present in more than 25% of the indexed files of its language family AND
    /// in at least five files (spec section 6, B1). SINGLE-SOURCE and NAMING skip such a family
    /// (a framework or house skeleton repeats by design); SHARED-PRIMITIVE keeps it. The spec
    /// names this flag but omits it from the struct; see the build notes.
    pub house_pattern: bool,
}

/// Index-building knobs. [`CloneOptions::default`] holds the spec's values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneOptions {
    /// The index floor and the winnowing guarantee length `T`: every pair of spans sharing at
    /// least this many consecutive Structural tokens is found. Spec: 40. Values below [`K`]
    /// behave as `K`.
    pub min_tokens: u32,
    /// A bucket present in more than this many distinct files is a framework or house pattern
    /// and is dropped and counted. Spec: 50.
    pub max_members: u32,
    /// The most candidate pairs whose match is extended and verified; spending it with an
    /// unverified candidate left sets `truncated`. A candidate inside a region an earlier
    /// extension already matched costs nothing and does not count. Spec: 200_000.
    pub max_pairs: u32,
    /// The most bytes the fingerprint index may hold (16 per fingerprint); hitting it stops
    /// indexing and sets `truncated`. Spec: 64 MiB.
    pub max_index_bytes: usize,
}

impl Default for CloneOptions {
    fn default() -> Self {
        Self {
            min_tokens: 40,
            max_members: 50,
            max_pairs: 200_000,
            max_index_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Why a file was not tokenized (listed in [`CloneIndex::skipped`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// Not TypeScript, TSX, JavaScript, Python or Rust.
    NotSource,
    /// A test, fixture, mock, story, e2e or snapshot file.
    TestPath,
    /// Generated code (marker, `.gen.`/`.pb.`/`_pb2.` name, `generated/` folder), a
    /// declaration file (`.d.ts`, `.pyi`), a committed `tsc` emit, and (by build-note decision)
    /// a migration or seed file.
    Generated,
    /// Vendored or build-output code (`vendor`, `node_modules`, `dist`, ...).
    Vendored,
    /// Minified or bundled code.
    Minified,
    /// An i18n catalog, or a data module (a file made only of object/array literals).
    I18nCatalog,
    /// More than 20,000 tokens: a bundle or a generated table.
    TooLarge,
    /// The file could not be tokenized.
    ParseFailed,
}

/// The result of one [`detect`] run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneIndex {
    /// Sorted by `(key, token_len)`; deterministic.
    pub families: Vec<CloneFamily>,
    /// Files tokenized and indexed.
    pub files_read: u32,
    /// Every input file that was not indexed, in path order.
    pub skipped: Vec<(String, SkipReason)>,
    /// Buckets over `max_members` (spec section 4, B1).
    pub boilerplate_buckets_skipped: u32,
    /// A cap fired (index bytes, pair count); rules treat the index as "not fully indexed".
    pub truncated: bool,
}

/// One position at which two Structural-equal spans differ under Exact normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disagreement {
    /// Token offset inside the span.
    pub position: u32,
    pub line_a: u32,
    pub line_b: u32,
    /// Exact-mode text of the token in span A (a property name, a literal value, an operator).
    pub a: String,
    pub b: String,
}

/// The tokens of `span` inside its item.
pub fn span_tokens<'a>(item: &'a ItemTokens, span: &CodeSpan) -> &'a [Token] {
    let start = span.token_start as usize;
    let end = start.saturating_add(span.token_len as usize);
    item.tokens.get(start..end).unwrap_or(&[])
}

/// Exact-mode comparison of two spans known to be Structural-equal. Empty when they agree.
///
/// `a` and `b` must come from an [`TokenMode::Exact`] tokenization of the spans' files (the
/// span's `item` indexes the result of [`tokens`]). Tokens are compared by id; the text each
/// side reports is its Exact text. A span that runs past its item, or two spans of different
/// length, are reported as disagreements at the positions that cannot be compared rather than
/// treated as agreeing.
pub fn exact_diff(
    a: &ItemTokens,
    a_span: &CodeSpan,
    b: &ItemTokens,
    b_span: &CodeSpan,
) -> Vec<Disagreement> {
    let a_start = a_span.token_start as usize;
    let b_start = b_span.token_start as usize;
    let len = a_span.token_len.max(b_span.token_len) as usize;
    let mut out = Vec::new();
    for offset in 0..len {
        let in_a = (offset < a_span.token_len as usize)
            .then(|| a.tokens.get(a_start + offset))
            .flatten();
        let in_b = (offset < b_span.token_len as usize)
            .then(|| b.tokens.get(b_start + offset))
            .flatten();
        let same = match (in_a, in_b) {
            (Some(x), Some(y)) => x.id == y.id && x.class == y.class,
            _ => false,
        };
        if same {
            continue;
        }
        out.push(Disagreement {
            position: u32::try_from(offset).unwrap_or(u32::MAX),
            line_a: in_a.map_or(0, |t| t.line),
            line_b: in_b.map_or(0, |t| t.line),
            a: describe(a, a_start + offset, in_a),
            b: describe(b, b_start + offset, in_b),
        });
    }
    out
}

/// The text a [`Disagreement`] reports for one side: the recorded Exact text, or the token id in
/// hex when the item was not tokenized in Exact mode, or empty when the span ran out.
fn describe(item: &ItemTokens, index: usize, token: Option<&Token>) -> String {
    match (token, item.texts.get(index)) {
        (Some(_), Some(text)) => text.clone(),
        (Some(token), None) => format!("#{:08x}", token.id),
        (None, _) => String::new(),
    }
}
