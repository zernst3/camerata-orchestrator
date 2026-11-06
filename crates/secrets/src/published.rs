//! T3: keys a vendor publishes in its own documentation. Stored as SHA-256 digests, never as
//! literals, so the repository contains no secret-shaped string (secret scanners and GitHub push
//! protection match by shape) and so a reader cannot mistake an entry for a leaked credential.

/// One published documentation value.
pub struct PublishedExample {
    /// The sha256 hex of the full value.
    pub digest: &'static str,
    /// The vendor page that publishes it.
    pub source: &'static str,
    /// The identifier the vendor documents it under, when the value is only a published example
    /// of that one setting (a config enum that reads as a password). The value is readable under
    /// that identifier and flagged under any other. `None` for a key the vendor publishes as
    /// itself (a documentation key every snippet uses), readable under any name.
    pub identifier: Option<&'static str>,
}

/// A key the vendor publishes as itself: readable under any name.
const fn key(digest: &'static str, source: &'static str) -> PublishedExample {
    PublishedExample {
        digest,
        source,
        identifier: None,
    }
}

/// A value the vendor documents as the value of one setting: readable under that name only.
const fn setting(
    digest: &'static str,
    source: &'static str,
    identifier: &'static str,
) -> PublishedExample {
    PublishedExample {
        digest,
        source,
        identifier: Some(identifier),
    }
}

pub const PUBLISHED_EXAMPLES: &[PublishedExample] = &[
    // Stripe's documented test-mode secret key, the one every Stripe docs snippet uses.
    key(
        "2cafc0970149a84f3b9e62eaf169f36f59907a3b3e31f7b82e68c69cd27f7326",
        "https://docs.stripe.com/keys",
    ),
    // GitHub's documented personal-access-token format example.
    key(
        "3543c50b0cb9cea8e55eb1f529b79f043cb250d485fde7b66cabbb5183add7da",
        "https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/about-authentication-to-github",
    ),
    // The Azure Storage emulator (Azurite) well-known development account key, which every
    // local-development connection string uses.
    key(
        "8b5130ead464e35d5743246136340bfe7e022bfaf8bb42ff07000e2acc5994ee",
        "https://learn.microsoft.com/azure/storage/common/storage-use-emulator",
    ),
    // Supabase's documented `password_requirements` setting value, a config enum that reads as a
    // password on a `password`-named identifier. It is readable under that setting's name only:
    // the same words assigned to `db_password` are a password. The shorter documented values are
    // under the length floor of the named-literal shape, so only these two need an entry.
    setting(
        "cfaccf9994dab33b04e075b70022663f8f9a298d8728c7cfb9364617cfd9f078",
        "https://supabase.com/docs/guides/local-development/cli/config#auth.password_requirements",
        "password_requirements",
    ),
    // The strongest documented `password_requirements` value (the same setting).
    setting(
        "cf8b615a70ffe8bb3a5f975aa39ad9e74ac720a16e332bd3fff4351fd930b20b",
        "https://supabase.com/docs/guides/local-development/cli/config#auth.password_requirements",
        "password_requirements",
    ),
    // AWS's two documented example access key ids contain the word EXAMPLE, so the T1
    // placeholder rule already keeps them readable; no digest is needed.
];

/// Whether `before_value` (the matched text in front of the value: the identifier and what sits
/// between it and the value) ends in `identifier` as a whole name. `password_requirements` is the
/// identifier in `auth.password_requirements = "` and in `password_requirements: "`, and it is not
/// in `my_password_requirements = "` or `password_requirements_backup = "`.
fn names_identifier(before_value: &str, identifier: &str) -> bool {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let lower = before_value.to_ascii_lowercase();
    // The identifier is the last name in front of the value: trim the assignment (`=`, `:`, quotes,
    // spaces) off the end and read the name that is left.
    let name: &str = lower.trim_end_matches(|c: char| !is_name_char(c));
    let start = name.rfind(|c: char| !is_name_char(c)).map_or(0, |i| {
        i + name[i..].chars().next().map_or(1, char::len_utf8)
    });
    &name[start..] == identifier
}

/// Whether `sha256_hex` is the digest of a published documentation value that may be read under
/// the name `before_value` ends in (see [`PublishedExample::identifier`]).
pub fn is_published_example(sha256_hex: &str, before_value: &str) -> bool {
    PUBLISHED_EXAMPLES.iter().any(|example| {
        example.digest == sha256_hex
            && example
                .identifier
                .is_none_or(|identifier| names_identifier(before_value, identifier))
    })
}
