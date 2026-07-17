//! A dependency-light `{{PLACEHOLDER}}` substitution pass. No templating crate: the
//! skeleton's placeholders are a small, fixed set (app name, package name,
//! description, capture URL, year), so a plain string-replace pass is simpler and has
//! one fewer dependency than pulling in `handlebars`/`tinytemplate` for five tokens.

/// Replace every `{{KEY}}` occurrence in `content` with its paired value from
/// `subs`. Keys are matched literally (no whitespace tolerance inside the braces —
/// the skeleton's own template files control their exact spelling, so this is not a
/// user-facing template language).
pub(crate) fn substitute(content: &str, subs: &[(&str, &str)]) -> String {
    let mut out = content.to_string();
    for (key, value) in subs {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    out
}

/// Scan `content` for any remaining `{{OUR_PLACEHOLDER}}`-shaped token after
/// substitution. Used by `scaffold_skeleton`'s tests to assert every template
/// file's placeholders were actually filled in (a placeholder left over almost
/// always means a typo'd key between a template file and the substitution map, so
/// this is the harness's cheap safety net against silent drift between the two).
///
/// Deliberately narrow: only matches `{{` immediately followed by one or more
/// ASCII uppercase letters/digits/underscores and `}}`, with no leading `$` — our
/// own placeholder convention (`{{APP_NAME}}`, `{{YEAR}}`, ...). This is narrower
/// than a generic `{{...}}` scan on purpose: `.github/workflows/ci.yml` legitimately
/// contains GitHub Actions' own `${{ github.ref }}` expression syntax (lowercase,
/// dotted, space-padded, dollar-prefixed), which is not one of ours and must not be
/// flagged as a leftover.
#[cfg(test)]
pub(crate) fn leftover_placeholders(content: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{'
            && bytes[i + 1] == b'{'
            && (i == 0 || bytes[i - 1] != b'$')
        {
            let key_start = i + 2;
            let mut j = key_start;
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j > key_start && bytes[j..].starts_with(b"}}") {
                found.push(content[i..j + 2].to_string());
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_all_occurrences_of_each_key() {
        let content = "name={{APP_NAME}}; again={{APP_NAME}}; year={{YEAR}}";
        let out = substitute(content, &[("APP_NAME", "Trip Planner"), ("YEAR", "2026")]);
        assert_eq!(out, "name=Trip Planner; again=Trip Planner; year=2026");
    }

    #[test]
    fn unmatched_placeholders_are_left_alone_when_key_absent() {
        let content = "{{KNOWN}} and {{UNKNOWN}}";
        let out = substitute(content, &[("KNOWN", "x")]);
        assert_eq!(out, "x and {{UNKNOWN}}");
    }

    #[test]
    fn leftover_placeholders_finds_unfilled_tokens() {
        assert_eq!(
            leftover_placeholders("all filled in"),
            Vec::<String>::new()
        );
        assert_eq!(
            leftover_placeholders("has {{ONE}} and {{TWO}} left"),
            vec!["{{ONE}}".to_string(), "{{TWO}}".to_string()]
        );
    }

    #[test]
    fn leftover_placeholders_ignores_github_actions_expression_syntax() {
        // `${{ github.ref }}` is GitHub Actions' own templating syntax (lowercase,
        // dotted, space-padded, dollar-prefixed) — not one of ours, must not be
        // flagged as an unfilled placeholder.
        assert_eq!(
            leftover_placeholders("concurrency: ci-${{ github.ref }}"),
            Vec::<String>::new()
        );
        assert_eq!(
            leftover_placeholders("image: ${{ steps.meta.outputs.image }}"),
            Vec::<String>::new()
        );
    }
}
