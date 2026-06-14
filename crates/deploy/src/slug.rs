//! URL slug helper: lowercase, alphanumeric, hyphens only.
//!
//! Used to derive the local dev URL from an app name and the Azure Web App
//! name from an `AzureConfig`. Any character that is not a letter or digit is
//! collapsed into a single hyphen; leading and trailing hyphens are stripped.

/// Convert `s` into a URL-safe slug: lowercase letters, digits, and hyphens
/// only. Adjacent non-alphanumeric characters are collapsed into one hyphen.
/// Leading and trailing hyphens are removed.
///
/// # Examples
///
/// ```
/// use camerata_deploy::slug::to_slug;
/// assert_eq!(to_slug("My Expense Tracker"), "my-expense-tracker");
/// assert_eq!(to_slug("  hello---world  "), "hello-world");
/// assert_eq!(to_slug("ABC123"), "abc123");
/// assert_eq!(to_slug("!@#"), "");
/// ```
pub fn to_slug(s: &str) -> String {
    let mut slug = String::with_capacity(s.len());
    let mut last_was_hyphen = true; // treat leading non-alphanum as a hyphen to avoid a leading dash

    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }

    // Strip trailing hyphen.
    if slug.ends_with('-') {
        slug.pop();
    }

    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_ascii_words() {
        assert_eq!(to_slug("My Expense Tracker"), "my-expense-tracker");
    }

    #[test]
    fn already_clean() {
        assert_eq!(to_slug("hello-world"), "hello-world");
    }

    #[test]
    fn all_uppercase() {
        assert_eq!(to_slug("ABC123"), "abc123");
    }

    #[test]
    fn leading_trailing_spaces() {
        assert_eq!(to_slug("  hello  "), "hello");
    }

    #[test]
    fn consecutive_separators_collapse() {
        assert_eq!(to_slug("hello---world"), "hello-world");
        assert_eq!(to_slug("a  b  c"), "a-b-c");
    }

    #[test]
    fn only_special_chars_yields_empty() {
        assert_eq!(to_slug("!@#"), "");
    }

    #[test]
    fn empty_string() {
        assert_eq!(to_slug(""), "");
    }

    #[test]
    fn mixed_separators_and_digits() {
        assert_eq!(to_slug("App v2.0 (beta)"), "app-v2-0-beta");
    }

    #[test]
    fn leading_separators_stripped() {
        assert_eq!(to_slug("---leading"), "leading");
    }
}
