//! `SupabaseServiceRoleClientChecker`: `SUPABASE-KEY-SERVICE-ROLE-CLIENT-1`, the deterministic
//! floor port of the rule's environment-variable prong.
//!
//! Frameworks inline every variable carrying a client prefix (`NEXT_PUBLIC_`, `VITE_`,
//! `REACT_APP_`, `EXPO_PUBLIC_` and similar) into the shipped JavaScript at build time. A
//! service-role or secret Supabase key behind such a variable therefore reaches every visitor's
//! browser, however safely the value itself is loaded. This checker reads `.env*` files and the
//! common build configuration files and flags a line that puts a client prefix on:
//!
//! - a variable whose NAME contains `SERVICE_ROLE` or `SB_SECRET` (the name alone ships the key,
//!   so the value is not consulted and an empty value still fires), or
//! - a value with the Supabase service-role or secret key shape that `camerata-secrets`
//!   recognises (a service-role JWT or an `sb_secret_` string), under any client-prefixed name.
//!
//! The anon / publishable key is designed to ship in the browser and is never flagged. A
//! server-only variable (no client prefix) is the correct place for the service-role key and
//! does not fire either.
//!
//! The rule's second prong, a service-role-shaped value referenced from frontend source, is
//! not ported: whether a source file reaches the bundle is a judgement. The checker therefore
//! reports itself as [`ArchChecker::advisory_coexisting`], which keeps the rule in the model's
//! prompt for that prong, while its own findings stay deterministic facts
//! ([`ArchChecker::defects_need_review`] is `false`).
//!
//! A finding never carries the key. Its `object` (the finding's snippet) is the variable name,
//! and the message names the file, line and variable, never the value.

use camerata_secrets::{detect_shapes, SecretShape};

use crate::arch_checker::{
    ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_CRITICAL,
};

pub const RULE_SERVICE_ROLE_CLIENT: &str = "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1";

const RULE_IDS: &[&str] = &[RULE_SERVICE_ROLE_CLIENT];

/// `.env*` files and the build configuration files that name client-bundled variables.
const INTEREST_GLOBS: &[&str] = &[
    "**/.env*",
    "**/next.config.*",
    "**/vite.config.*",
    "**/nuxt.config.*",
    "**/app.config.*",
    "**/app.json",
    "**/eas.json",
    "**/vercel.json",
    "**/netlify.toml",
];

/// Variable-name prefixes a framework inlines into the browser bundle.
const CLIENT_PREFIXES: &[&str] = &[
    "NEXT_PUBLIC_",
    "VITE_",
    "REACT_APP_",
    "EXPO_PUBLIC_",
    "GATSBY_",
    "NUXT_PUBLIC_",
    "VUE_APP_",
    "PLASMO_PUBLIC_",
    "NG_APP_",
    "PUBLIC_",
];

/// Name fragments that mark a service-role or secret Supabase key.
const SECRET_NAME_FRAGMENTS: &[&str] = &["SERVICE_ROLE", "SB_SECRET"];

/// The shapes `camerata-secrets` knows for the service-role / secret Supabase key.
const SECRET_SHAPES: &[SecretShape] = &[
    SecretShape::SupabaseSecretKey,
    SecretShape::SupabaseServiceRoleJwt,
];

pub struct SupabaseServiceRoleClientChecker;

impl ArchChecker for SupabaseServiceRoleClientChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn advisory_coexisting(&self) -> bool {
        true
    }

    fn defects_need_review(&self) -> bool {
        false
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let mut out = Vec::new();
        for (path, content) in repo.files {
            if path.split('/').any(|s| s == "node_modules")
                || !crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path)
            {
                continue;
            }
            let is_env = path
                .rsplit('/')
                .next()
                .is_some_and(|f| f.starts_with(".env"));
            for (index, line) in content.lines().enumerate() {
                let hit = if is_env {
                    check_env_line(line)
                } else {
                    check_config_line(line)
                };
                if let Some(hit) = hit {
                    out.push(violation(path, index + 1, &hit));
                }
            }
        }
        out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        out
    }
}

/// What a line was flagged for.
struct Hit {
    /// The client-prefixed variable name (a name is not a secret).
    name: String,
    /// Whether the NAME marks the key (`true`) or only the VALUE has the key shape (`false`).
    by_name: bool,
}

fn violation(path: &str, line: usize, hit: &Hit) -> ArchViolation {
    let why = if hit.by_name {
        "its name carries a client-bundle prefix together with SERVICE_ROLE or SB_SECRET, so the build inlines the key into the shipped JavaScript"
    } else {
        "its value has the shape of a Supabase service-role or secret key, and the client-bundle prefix on its name makes the build inline it into the shipped JavaScript"
    };
    ArchViolation {
        grade: ViolationGrade::Defect,
        rule_id: RULE_SERVICE_ROLE_CLIENT.to_string(),
        file: path.to_string(),
        line,
        object: Some(hit.name.clone()),
        severity: SEVERITY_CRITICAL,
        message: format!(
            "`{}` ({path}:{line}) puts a Supabase service-role / secret key behind a client-bundled variable: {why}. \
             That key bypasses every Row Level Security policy, so every visitor to the site can read and write the \
             project's data. Rotate the key in the Supabase dashboard now (treat it as already exposed), remove the \
             client prefix, and keep the key in a server-only variable used from a server route or Edge Function. \
             The anon / publishable key is the one designed to ship in the browser and is not affected.",
            hit.name
        ),
    }
}

/// `NAME=value` (optionally `export NAME=value`), comments and blank lines skipped.
fn check_env_line(line: &str) -> Option<Hit> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let trimmed = trimmed
        .strip_prefix("export ")
        .map_or(trimmed, str::trim_start);
    let (name, value) = trimmed.split_once('=')?;
    let name = name.trim();
    if !is_identifier(name) || !has_client_prefix(name) {
        return None;
    }
    if name_marks_secret(name) {
        return Some(Hit {
            name: name.to_string(),
            by_name: true,
        });
    }
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
    value_has_secret_shape(value).then(|| Hit {
        name: name.to_string(),
        by_name: false,
    })
}

/// A configuration line (`process.env.NEXT_PUBLIC_X`, a JSON key, a TOML key): any identifier
/// token with a client prefix, flagged by name, or by a key-shaped value on the same line.
fn check_config_line(line: &str) -> Option<Hit> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return None;
    }
    if trimmed.starts_with('#') {
        return None;
    }
    let prefixed: Vec<&str> = line
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty() && has_client_prefix(t))
        .collect();
    if let Some(name) = prefixed.iter().find(|t| name_marks_secret(t)) {
        return Some(Hit {
            name: (*name).to_string(),
            by_name: true,
        });
    }
    let first = prefixed.first()?;
    value_has_secret_shape(line).then(|| Hit {
        name: (*first).to_string(),
        by_name: false,
    })
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn has_client_prefix(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    CLIENT_PREFIXES.iter().any(|p| upper.starts_with(p))
}

fn name_marks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAME_FRAGMENTS.iter().any(|f| upper.contains(f))
}

fn value_has_secret_shape(text: &str) -> bool {
    detect_shapes(text, SECRET_SHAPES)
        .iter()
        .any(camerata_secrets::SecretMatch::is_secret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_secrets::fixtures::{jwt, planted};

    fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
            .collect()
    }

    fn run(f: &[(String, String)]) -> Vec<ArchViolation> {
        SupabaseServiceRoleClientChecker.check(&RepoView {
            spec: "test/repo",
            files: f,
        })
    }

    #[test]
    fn the_fixture_shape_env_production_with_a_client_prefixed_service_role_key_fires_once() {
        let secret = planted(SecretShape::SupabaseServiceRoleJwt).value;
        let env = format!(
            "# Production environment configuration\n\nNEXT_PUBLIC_SUPABASE_URL=https://abcdefghij.supabase.co\nNEXT_PUBLIC_SUPABASE_ANON_KEY=anon\n\nNEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY={secret}\n"
        );
        let f = files(&[(".env.production", &env)]);
        let vs = run(&f);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        let v = &vs[0];
        assert_eq!(v.rule_id, RULE_SERVICE_ROLE_CLIENT);
        assert_eq!(v.file, ".env.production");
        assert_eq!(v.line, 6);
        assert_eq!(v.severity, SEVERITY_CRITICAL);
        assert_eq!(v.grade, ViolationGrade::Defect);
        assert_eq!(
            v.object.as_deref(),
            Some("NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY")
        );
        assert!(
            !v.message.contains(&secret) && !v.object.as_deref().unwrap().contains(&secret),
            "the finding never carries the key"
        );
    }

    #[test]
    fn a_server_only_service_role_variable_does_not_fire() {
        let secret = planted(SecretShape::SupabaseServiceRoleJwt).value;
        let env = format!(
            "NEXT_PUBLIC_SUPABASE_URL=https://abcdefghij.supabase.co\nSUPABASE_SERVICE_ROLE_KEY={secret}\nSB_SECRET_KEY={}\n",
            planted(SecretShape::SupabaseSecretKey).value
        );
        let f = files(&[
            (".env.production", &env),
            (".env.example", "SUPABASE_SERVICE_ROLE_KEY=\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn the_name_alone_fires_even_with_an_empty_or_placeholder_value() {
        let f = files(&[(
            ".env.example",
            "VITE_SUPABASE_SERVICE_ROLE_KEY=\nEXPO_PUBLIC_SB_SECRET=\"your-key-here\"\n",
        )]);
        let vs = run(&f);
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert_eq!(vs[0].line, 1);
        assert_eq!(vs[1].line, 2);
    }

    #[test]
    fn a_key_shaped_value_under_a_client_prefixed_name_fires_whatever_the_name() {
        let by_jwt = format!("NEXT_PUBLIC_SUPABASE_KEY={}\n", jwt(Some("service_role")));
        let by_sb = format!(
            "export VITE_ADMIN_TOKEN='{}'\n",
            planted(SecretShape::SupabaseSecretKey).value
        );
        let f = files(&[(".env.local", &by_jwt), (".env.staging", &by_sb)]);
        let vs = run(&f);
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs.iter().all(|v| !v.message.contains("eyJ")));
    }

    #[test]
    fn the_anon_key_under_a_client_prefix_is_never_flagged() {
        let env = format!(
            "NEXT_PUBLIC_SUPABASE_ANON_KEY={}\nNEXT_PUBLIC_SUPABASE_PUBLISHABLE_KEY=sb_publishable_abcdefghijklmnop\n",
            jwt(Some("anon"))
        );
        let f = files(&[(".env.production", &env)]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn a_commented_line_does_not_fire() {
        let f = files(&[(".env", "# NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY=x\n")]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn a_config_file_naming_a_client_prefixed_service_role_variable_fires() {
        let f = files(&[
            (
                "next.config.mjs",
                "export default {\n  env: {\n    NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY: process.env.SUPABASE_SERVICE_ROLE_KEY,\n  },\n};\n",
            ),
            (
                "vite.config.ts",
                "// VITE_SUPABASE_SERVICE_ROLE_KEY is read on the server\nexport default {};\n",
            ),
            (
                "server/next.config.js",
                "module.exports = { serverKey: process.env.SUPABASE_SERVICE_ROLE_KEY };\n",
            ),
        ]);
        let vs = run(&f);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].file, "next.config.mjs");
        assert_eq!(vs[0].line, 3);
    }

    #[test]
    fn it_is_deterministic_and_keeps_the_rule_in_the_model_prompt_for_the_source_prong() {
        let c = SupabaseServiceRoleClientChecker;
        assert!(c.advisory_coexisting());
        assert!(!c.defects_need_review());
        assert_eq!(c.rule_ids(), &[RULE_SERVICE_ROLE_CLIENT]);
    }

    #[test]
    fn it_is_registered_in_the_checker_registry() {
        assert!(crate::arch_checker::all_checkers()
            .iter()
            .any(|c| c.rule_ids().contains(&RULE_SERVICE_ROLE_CLIENT)));
    }
}
