//! `applies_to`: which files a rule can possibly apply to (token-efficiency rank 7).
//!
//! # Why a rule declares this
//!
//! The AI audit sends every selected rule with every chunk of code. Most rules are about one
//! kind of file (a Row Level Security rule is about SQL migrations, a CI rule is about pipeline
//! files), so the model reads code the rule provably cannot match, and the client pays for it.
//! A rule that declares where it applies lets the router (`camerata-server`'s `scan_routing`)
//! send it only with the files it can match.
//!
//! # The shape
//!
//! ```toml
//! applies_to = { paths = ["**/*.sql", "supabase/**"], languages = ["rust"], contains_any = ["create policy"] }
//! ```
//!
//! * `paths`: repo-relative file globs. `*` matches within one path segment, a whole `**`
//!   segment matches any number of segments (the matcher is `camerata_checks::arch_checker::
//!   glob_match`; nothing else is supported, so nothing else is accepted).
//! * `languages`: values from [`ROUTING_LANGUAGES`], the languages the scan recognises from a
//!   file's extension.
//! * `contains_any`: case-insensitive substrings. A file whose text contains ANY of them is in
//!   scope whatever its path or language. This is how a rule about SQL statements still reaches
//!   SQL embedded in a `.ts`, `.py` or `.rb` migration. The router reads the file text the scan
//!   has already loaded; there is no extra I/O.
//! * A file matches when it matches ANY declared path OR is in ANY declared language OR contains
//!   ANY declared marker (a union: a rule is only ever sent to MORE files by adding an entry,
//!   never fewer).
//!
//! An absent `applies_to` means every file, exactly as before this field existed.
//!
//! # Validation is the loader's job, and it is loud
//!
//! A scope that silently matches nothing would hide every finding of its rule while the report
//! called the rule clean. So a value this module cannot honour is a LOAD ERROR (it reaches the
//! caller as a TOML parse error naming the file): an unknown language, an unknown key, a glob
//! using syntax the matcher does not implement (`?`, `[a-z]`, `{a,b}`, `!`), an absolute or
//! `..` path, an empty list, an empty `applies_to`. The router adds one more safety on top: a
//! file whose language cannot be determined is treated as a match (see `scan_routing`).

use serde::de::{self, Deserializer};
use serde::Deserialize;

/// The language keys a file extension can resolve to. This is the closed vocabulary for
/// `applies_to.languages`; `camerata-server`'s `scan_routing` resolves a file to one of these
/// through [`lang_for_extension`], so the list and the resolver cannot drift.
pub const ROUTING_LANGUAGES: &[&str] = &[
    "cpp", "csharp", "go", "java", "kotlin", "php", "python", "ruby", "rust", "swift", "web",
];

/// Map a file extension (lowercase, no dot) to its canonical language key, if known.
///
/// `web` is the whole JS/TS/React ecosystem (they share frontend rules), module-flavoured
/// extensions included. `csharp` includes Razor views. Config and data extensions (toml, json,
/// yaml, sql, sh) are not a routed language: they return `None`.
pub fn lang_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" | "vue" | "svelte"
        | "astro" => "web",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "cs" | "cshtml" | "razor" => "csharp",
        "swift" => "swift",
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        _ => return None,
    })
}

/// A rule's declared scope. Constructed only through [`AppliesTo::new`] or deserialization,
/// both of which validate, so a value of this type is always honourable by the router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliesTo {
    /// Repo-relative file globs, in authored order. May be empty when `languages` is not.
    pub paths: Vec<String>,
    /// Language keys from [`ROUTING_LANGUAGES`], lowercase, in authored order.
    pub languages: Vec<String>,
    /// Content markers, lowercase: a file whose text contains any of them is in scope.
    pub contains_any: Vec<String>,
}

impl AppliesTo {
    /// Validate and build. `Err` is a message naming the offending value and the valid choices.
    pub fn new(
        paths: Vec<String>,
        languages: Vec<String>,
        contains_any: Vec<String>,
    ) -> Result<Self, String> {
        if paths.is_empty() && languages.is_empty() && contains_any.is_empty() {
            return Err(
                "applies_to declares none of `paths`, `languages` or `contains_any`; \
                 omit the field to mean every file"
                    .to_string(),
            );
        }
        let contains_any = contains_any
            .into_iter()
            .map(|m| {
                if m.trim().is_empty() {
                    Err("applies_to contains_any has a blank marker".to_string())
                } else if m.contains('\n') || m.contains('\r') {
                    Err(format!(
                        "applies_to contains_any marker `{}` spans lines; a marker is one line \
                         of text",
                        m.escape_debug()
                    ))
                } else {
                    Ok(m.to_lowercase())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let paths = paths
            .into_iter()
            .map(|p| validate_glob(&p).map(|()| p))
            .collect::<Result<Vec<_>, _>>()?;
        let languages = languages
            .into_iter()
            .map(|l| {
                let lower = l.trim().to_ascii_lowercase();
                if ROUTING_LANGUAGES.contains(&lower.as_str()) {
                    Ok(lower)
                } else {
                    Err(format!(
                        "applies_to language `{l}` is not recognised; the languages are: {}",
                        ROUTING_LANGUAGES.join(", ")
                    ))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            paths,
            languages,
            contains_any,
        })
    }

    /// A canonical, order-independent key: two scopes with the same key route identically, so
    /// the router can group the rules that share one.
    pub fn canonical_key(&self) -> String {
        let mut paths: Vec<&str> = self.paths.iter().map(String::as_str).collect();
        paths.sort_unstable();
        paths.dedup();
        let mut languages: Vec<&str> = self.languages.iter().map(String::as_str).collect();
        languages.sort_unstable();
        languages.dedup();
        let mut markers: Vec<&str> = self.contains_any.iter().map(String::as_str).collect();
        markers.sort_unstable();
        markers.dedup();
        format!(
            "paths={};languages={};contains_any={}",
            paths.join(","),
            languages.join(","),
            markers.join("\u{1f}")
        )
    }
}

/// Reject a glob the matcher would silently treat as a literal (and so match nothing).
fn validate_glob(glob: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("applies_to path `{glob}` is invalid: {why}"));
    if glob.trim().is_empty() {
        return bad("it is empty");
    }
    if glob != glob.trim() {
        return bad("it has leading or trailing whitespace");
    }
    if glob.starts_with('/') {
        return bad("paths are repo-relative, so it must not start with `/`");
    }
    if glob.contains('\\') {
        return bad("use `/` as the separator");
    }
    if let Some(c) = glob
        .chars()
        .find(|c| matches!(c, '?' | '[' | ']' | '{' | '}' | '!'))
    {
        return bad(&format!(
            "`{c}` is not supported (only `*` within a segment and a whole `**` segment are)"
        ));
    }
    for segment in glob.split('/') {
        if segment.is_empty() {
            return bad("it has an empty segment (a doubled or trailing `/`)");
        }
        if segment == "." || segment == ".." {
            return bad("`.` and `..` segments are not allowed");
        }
        if segment.contains("**") && segment != "**" {
            return bad("`**` must be a whole segment (e.g. `src/**/*.sql`)");
        }
    }
    Ok(())
}

/// The TOML shape of `applies_to`. `deny_unknown_fields` so an unrecognised key (say a future
/// `frameworks`, or a typo of `paths`) is a load error, never an ignored scope.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppliesToToml {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    contains_any: Vec<String>,
}

impl<'de> Deserialize<'de> for AppliesTo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = AppliesToToml::deserialize(deserializer)?;
        AppliesTo::new(raw.paths, raw.languages, raw.contains_any).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<AppliesTo, String> {
        #[derive(Deserialize)]
        struct W {
            applies_to: AppliesTo,
        }
        toml::from_str::<W>(src)
            .map(|w| w.applies_to)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_valid_scope_parses_and_normalises_language_case() {
        let a =
            parse(r#"applies_to = { paths = ["**/*.sql", "supabase/**"], languages = ["Rust"] }"#)
                .unwrap();
        assert_eq!(a.paths, vec!["**/*.sql", "supabase/**"]);
        assert_eq!(a.languages, vec!["rust"]);
    }

    #[test]
    fn an_unknown_language_is_a_load_error_naming_the_choices() {
        let err = parse(r#"applies_to = { languages = ["rustt"] }"#).unwrap_err();
        assert!(err.contains("rustt") && err.contains("rust, "), "{err}");
    }

    #[test]
    fn an_unknown_key_is_a_load_error() {
        let err = parse(r#"applies_to = { frameworks = ["next"] }"#).unwrap_err();
        assert!(err.contains("frameworks"), "{err}");
        assert!(parse(r#"applies_to = { path = ["**/*.sql"] }"#).is_err());
    }

    #[test]
    fn an_empty_scope_is_a_load_error_never_matches_nothing() {
        assert!(parse("applies_to = {}").is_err());
        assert!(parse("applies_to = { paths = [], languages = [] }").is_err());
    }

    #[test]
    fn globs_the_matcher_cannot_honour_are_load_errors() {
        for bad in [
            "",
            "/abs/**",
            "src\\\\x",
            "src/../x",
            "./x",
            "a//b",
            "src/",
            "**.sql",
            "src/*.{sql,toml}",
            "src/file?.sql",
            "src/[ab].sql",
            "!src/**",
            " src/**",
        ] {
            let src = format!("applies_to = {{ paths = [{bad:?}] }}");
            assert!(parse(&src).is_err(), "`{bad}` must be rejected");
        }
        for good in [
            "**/*.sql",
            "supabase/**",
            "*.toml",
            ".github/**",
            "a/b/c.txt",
        ] {
            let src = format!("applies_to = {{ paths = [{good:?}] }}");
            assert!(parse(&src).is_ok(), "`{good}` must be accepted");
        }
    }

    #[test]
    fn contains_any_markers_are_lowercased_and_validated() {
        let a = parse(r#"applies_to = { contains_any = ["CREATE Policy", "grant "] }"#).unwrap();
        assert_eq!(a.contains_any, vec!["create policy", "grant "]);
        assert!(a.paths.is_empty() && a.languages.is_empty());
        assert!(parse(r#"applies_to = { contains_any = [] }"#).is_err());
        assert!(parse(r#"applies_to = { contains_any = ["  "] }"#).is_err());
        assert!(parse(r#"applies_to = { contains_any = ["a\nb"] }"#).is_err());
    }

    #[test]
    fn contains_any_is_part_of_the_canonical_key() {
        let a = AppliesTo::new(vec![], vec![], vec!["x".into(), "y".into()]).unwrap();
        let b = AppliesTo::new(vec![], vec![], vec!["y".into(), "x".into()]).unwrap();
        let c = AppliesTo::new(vec![], vec![], vec!["x".into()]).unwrap();
        assert_eq!(a.canonical_key(), b.canonical_key());
        assert_ne!(a.canonical_key(), c.canonical_key());
    }

    #[test]
    fn the_canonical_key_ignores_order_and_duplicates() {
        let a = AppliesTo::new(
            vec!["b/**".into(), "a/**".into()],
            vec!["rust".into()],
            vec![],
        )
        .unwrap();
        let b = AppliesTo::new(
            vec!["a/**".into(), "b/**".into(), "a/**".into()],
            vec!["rust".into()],
            vec![],
        )
        .unwrap();
        assert_eq!(a.canonical_key(), b.canonical_key());
        let c = AppliesTo::new(vec!["a/**".into()], vec!["rust".into()], vec![]).unwrap();
        assert_ne!(a.canonical_key(), c.canonical_key());
    }

    #[test]
    fn every_language_the_resolver_can_return_is_in_the_vocabulary() {
        for ext in [
            "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "py", "go", "java", "kt",
            "rb", "php", "cs", "cshtml", "razor", "swift", "c", "h", "cpp",
        ] {
            let lang = lang_for_extension(ext).unwrap();
            assert!(ROUTING_LANGUAGES.contains(&lang), "{lang}");
        }
        for lang in ROUTING_LANGUAGES {
            assert!(
                ["rs", "ts", "py", "go", "java", "kt", "rb", "php", "cs", "swift", "c"]
                    .iter()
                    .any(|e| lang_for_extension(e) == Some(*lang)),
                "{lang} is declared but no extension resolves to it"
            );
        }
    }
}
