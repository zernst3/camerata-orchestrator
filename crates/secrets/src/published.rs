//! T3: keys a vendor publishes in its own documentation. Stored as SHA-256 digests, never as
//! literals, so the repository contains no secret-shaped string (secret scanners and GitHub push
//! protection match by shape) and so a reader cannot mistake an entry for a leaked credential.

/// `(sha256 hex of the full key, the vendor page that publishes it)`.
pub const PUBLISHED_EXAMPLES: &[(&str, &str)] = &[
    (
        // Stripe's documented test-mode secret key, the one every Stripe docs snippet uses.
        "2cafc0970149a84f3b9e62eaf169f36f59907a3b3e31f7b82e68c69cd27f7326",
        "https://docs.stripe.com/keys",
    ),
    (
        // GitHub's documented personal-access-token format example.
        "3543c50b0cb9cea8e55eb1f529b79f043cb250d485fde7b66cabbb5183add7da",
        "https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/about-authentication-to-github",
    ),
    // AWS's two documented example access key ids contain the word EXAMPLE, so the T1
    // placeholder rule already keeps them readable; no digest is needed.
];

/// Whether `sha256_hex` is the digest of a published documentation key.
pub fn is_published_example(sha256_hex: &str) -> bool {
    PUBLISHED_EXAMPLES
        .iter()
        .any(|(digest, _)| *digest == sha256_hex)
}
