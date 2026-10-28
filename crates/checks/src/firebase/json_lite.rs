//! A small, lenient JSON reader that keeps line numbers.
//!
//! Two Firebase files are JSON that people edit by hand: `firebase.json` and the Realtime
//! Database rules file. firebase-tools reads both with comments allowed, and the findings need
//! the line of a key (`".write": true` at line 9), which `serde_json` does not give. This reader
//! accepts `//` and `/* */` comments and trailing commas, keeps object keys in source order with
//! their line, and reports every place it could not account for the input instead of guessing.
//!
//! # Hardening contract
//!
//! The same as `supabase/splitter.rs`: the input is untrusted and possibly truncated, so this
//! never panics (it works on a `Vec<char>`, never a byte index), every loop advances, and nesting
//! is capped so a file of 100,000 `{` cannot overflow the stack. Anything it cannot read is a
//! [`JsonError`]; a caller treats any error as "the file could not be read" (degraded), never as
//! clean.

/// Deepest nesting read before the file is reported as unreadable.
const MAX_DEPTH: usize = 64;

/// One parse problem: where and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    pub line: usize,
    pub what: String,
}

/// A JSON value. Objects keep source order and each key's line.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Object(Vec<JsonMember>),
    Array(Vec<Json>),
    Str(String),
    Bool(bool),
    Number(f64),
    Null,
}

/// One `"key": value` member of an object.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonMember {
    pub key: String,
    /// 1-based line of the key.
    pub line: usize,
    pub value: Json,
}

impl Json {
    /// The member `key` of an object, if this is an object that has one.
    pub fn get(&self, key: &str) -> Option<&JsonMember> {
        match self {
            Json::Object(members) => members.iter().find(|m| m.key == key),
            _ => None,
        }
    }

    /// The string value, if this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Parse `text`. Returns the value read (best effort) and every error met; an empty error list
/// means the whole input was a single well-formed (comments and trailing commas allowed) value.
pub fn parse(text: &str) -> (Option<Json>, Vec<JsonError>) {
    let mut reader = Reader {
        chars: text.replace("\r\n", "\n").chars().collect(),
        i: 0,
        line: 1,
        errors: Vec::new(),
    };
    reader.skip_trivia();
    let value = reader.value(0);
    reader.skip_trivia();
    if reader.i < reader.chars.len() {
        reader.error("text after the end of the JSON value");
    }
    (value, reader.errors)
}

struct Reader {
    chars: Vec<char>,
    i: usize,
    line: usize,
    errors: Vec<JsonError>,
}

impl Reader {
    fn error(&mut self, what: &str) {
        self.errors.push(JsonError {
            line: self.line,
            what: what.to_string(),
        });
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.i + offset).copied()
    }

    /// Whitespace and comments. An unterminated block comment runs to EOF.
    fn skip_trivia(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                self.line += 1;
                self.i += 1;
            } else if c.is_whitespace() {
                self.i += 1;
            } else if c == '/' && self.peek_at(1) == Some('/') {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
            } else if c == '/' && self.peek_at(1) == Some('*') {
                self.i += 2;
                loop {
                    match self.peek() {
                        None => {
                            self.error("unterminated block comment");
                            break;
                        }
                        Some('*') if self.peek_at(1) == Some('/') => {
                            self.i += 2;
                            break;
                        }
                        Some('\n') => {
                            self.line += 1;
                            self.i += 1;
                        }
                        Some(_) => self.i += 1,
                    }
                }
            } else {
                break;
            }
        }
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            self.error("nesting too deep");
            // Stop reading: everything after this point is abandoned.
            self.i = self.chars.len();
            return None;
        }
        match self.peek()? {
            '{' => Some(self.object(depth)),
            '[' => Some(self.array(depth)),
            '"' | '\'' => self.string().map(Json::Str),
            c if c == '-' || c.is_ascii_digit() => Some(self.number()),
            c if c.is_ascii_alphabetic() => Some(self.word()),
            _ => {
                self.error("unexpected character");
                // Advance so the caller's loop terminates.
                self.i += 1;
                None
            }
        }
    }

    fn object(&mut self, depth: usize) -> Json {
        self.i += 1; // '{'
        let mut members = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => {
                    self.error("unterminated object");
                    break;
                }
                Some('}') => {
                    self.i += 1;
                    break;
                }
                Some(',') => {
                    // A stray or doubled comma: tolerated only as a separator.
                    self.i += 1;
                    continue;
                }
                Some(_) => {}
            }
            let line = self.line;
            let key = match self.peek() {
                Some('"' | '\'') => self.string(),
                _ => {
                    self.error("expected a quoted key");
                    // Skip one character so the loop advances.
                    self.i += 1;
                    continue;
                }
            };
            self.skip_trivia();
            if self.peek() == Some(':') {
                self.i += 1;
            } else {
                self.error("expected ':' after a key");
            }
            self.skip_trivia();
            let before = self.i;
            let value = self.value(depth + 1);
            if self.i == before {
                // The value reader made no progress; step over one character.
                self.i += 1;
            }
            if let (Some(key), Some(value)) = (key, value) {
                members.push(JsonMember { key, line, value });
            }
        }
        Json::Object(members)
    }

    fn array(&mut self, depth: usize) -> Json {
        self.i += 1; // '['
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => {
                    self.error("unterminated array");
                    break;
                }
                Some(']') => {
                    self.i += 1;
                    break;
                }
                Some(',') => {
                    self.i += 1;
                    continue;
                }
                Some(_) => {}
            }
            let before = self.i;
            if let Some(value) = self.value(depth + 1) {
                items.push(value);
            }
            if self.i == before {
                self.i += 1;
            }
        }
        Json::Array(items)
    }

    /// A quoted string with backslash escapes. An unterminated string runs to EOF.
    fn string(&mut self) -> Option<String> {
        let quote = self.peek()?;
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => {
                    self.error("unterminated string");
                    return Some(out);
                }
                Some(c) if c == quote => {
                    self.i += 1;
                    return Some(out);
                }
                Some('\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('u') => {
                            // Kept verbatim: the reader never needs the decoded code point.
                            out.push_str("\\u");
                        }
                        Some(c) => out.push(c),
                        None => {
                            self.error("unterminated string");
                            return Some(out);
                        }
                    }
                    if self.peek() == Some('\n') {
                        self.line += 1;
                    }
                    self.i += 1;
                }
                Some(c) => {
                    if c == '\n' {
                        self.line += 1;
                    }
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Json {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        {
            self.i += 1;
        }
        let text: String = self.chars[start..self.i].iter().collect();
        match text.parse::<f64>() {
            Ok(n) => Json::Number(n),
            Err(_) => {
                self.error("malformed number");
                Json::Number(0.0)
            }
        }
    }

    fn word(&mut self) -> Json {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric()) {
            self.i += 1;
        }
        let word: String = self.chars[start..self.i].iter().collect();
        match word.as_str() {
            "true" => Json::Bool(true),
            "false" => Json::Bool(false),
            "null" => Json::Null,
            _ => {
                self.error("unexpected word");
                Json::Null
            }
        }
    }
}
