//! [`FirebaseAdminSdkClientChecker`]: FIREBASE-ADMIN-SDK-CLIENT-1 (rule 11).
//!
//! The Admin SDK is for privileged server environments. Imported by code that ships to a browser
//! it either fails to build or ships the service-account credentials it is initialised with.
//! "Browser-bundled" is decided from the nearest `package.json` (a client bundler or SPA
//! framework and no server runtime), a `"use client"` directive, or a `public/` style folder.
//! A fullstack framework keeps server and client code in one package, so for those only the
//! directive flags a file (a false negative by design).

use super::functions::manifest_dependencies;
use super::RULE_ADMIN_SDK_CLIENT;
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, ViolationGrade, SEVERITY_HIGH};
use crate::extract::paths::is_test_path;
use crate::extract::{imports, lang_for_path};

const INTEREST_GLOBS: &[&str] = &[
    "**/package.json",
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.mjs",
    "**/*.cjs",
    "**/*.mts",
    "**/*.cts",
];

/// Dependencies that mean the package is bundled for a browser.
const CLIENT_BUNDLERS: &[&str] = &[
    "vite",
    "react-scripts",
    "@angular/core",
    "vue",
    "svelte",
    "parcel",
];

/// Dependencies that mean server code lives in the package, so the Admin SDK may be legitimate.
const SERVER_RUNTIMES: &[&str] = &[
    "next",
    "nuxt",
    "@sveltejs/kit",
    "astro",
    "express",
    "fastify",
    "koa",
    "hono",
    "firebase-functions",
];

/// The checker for the Admin SDK in browser code.
#[derive(Debug, Clone, Copy, Default)]
pub struct FirebaseAdminSdkClientChecker;

impl ArchChecker for FirebaseAdminSdkClientChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        &[RULE_ADMIN_SDK_CLIENT]
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let mut out = Vec::new();
        for (path, source) in repo.files {
            if is_test_path(path)
                || path.to_ascii_lowercase().ends_with(".d.ts")
                || path.split('/').any(|s| s == "node_modules")
            {
                continue;
            }
            let Some(lang) = lang_for_path(path) else {
                continue;
            };
            let Some(import) = imports(lang, source)
                .into_iter()
                .find(|i| is_admin_specifier(&i.specifier) && !i.type_only)
            else {
                continue;
            };
            let Some(reason) = browser_bundled(path, source, repo.files) else {
                continue;
            };
            out.push(ArchViolation {
                grade: ViolationGrade::Defect,
                rule_id: RULE_ADMIN_SDK_CLIENT.to_string(),
                file: path.clone(),
                line: import.line,
                object: None,
                message: format!(
                    "`{path}` (line {}) imports `firebase-admin`, and {reason}. The Admin SDK is for privileged server environments; in a browser bundle it either fails to build or ships the service-account credentials it is initialised with.",
                    import.line
                ),
                severity: SEVERITY_HIGH,
            });
        }
        out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        out
    }

    /// No `firebase-admin` import anywhere: nothing for this checker to answer.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        !repo.files.iter().any(|(path, source)| {
            lang_for_path(path).is_some_and(|lang| {
                imports(lang, source)
                    .iter()
                    .any(|i| is_admin_specifier(&i.specifier))
            })
        })
    }
}

fn is_admin_specifier(specifier: &str) -> bool {
    specifier == "firebase-admin" || specifier.starts_with("firebase-admin/")
}

/// Why this file is browser-bundled, or `None` when it is not (or cannot be shown to be).
fn browser_bundled(path: &str, source: &str, files: &[(String, String)]) -> Option<String> {
    if has_use_client(source) {
        return Some("this file is a Next.js client component (`\"use client\"`)".to_string());
    }
    if let Some(folder) = path
        .split('/')
        .find(|s| matches!(*s, "public" | "static" | "www"))
    {
        return Some(format!(
            "this file is under `{folder}/`, which is served to browsers"
        ));
    }
    let (manifest_dir, deps) = nearest_manifest(path, files)?;
    let has = |name: &str| deps.iter().any(|d| d == name);
    if SERVER_RUNTIMES.iter().any(|s| has(s))
        || deps
            .iter()
            .any(|d| d.starts_with("@remix-run/") || d.starts_with("@nestjs/"))
        || (has("react-router") && has_react_router_root(&manifest_dir, files))
    {
        return None;
    }
    let bundler = CLIENT_BUNDLERS
        .iter()
        .find(|b| has(b))
        .copied()
        .or_else(|| (has("webpack") && has("react")).then_some("webpack"))?;
    Some(format!(
        "this package is a browser application (`{bundler}` in `package.json`)"
    ))
}

/// The first statement of the file is the `"use client"` directive.
fn has_use_client(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') {
            continue;
        }
        return t.starts_with("\"use client\"") || t.starts_with("'use client'");
    }
    false
}

/// The dependency names of the `package.json` nearest above `path`, with its directory.
fn nearest_manifest(path: &str, files: &[(String, String)]) -> Option<(String, Vec<String>)> {
    files
        .iter()
        .filter(|(p, _)| p.rsplit('/').next() == Some("package.json"))
        .filter_map(|(p, c)| {
            let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
            let applies = dir.is_empty() || path.starts_with(&format!("{dir}/"));
            applies.then(|| (dir.to_string(), manifest_dependencies(c)))
        })
        .max_by_key(|(dir, _)| dir.len())
}

fn has_react_router_root(dir: &str, files: &[(String, String)]) -> bool {
    let prefix = if dir.is_empty() {
        "app/root.".to_string()
    } else {
        format!("{dir}/app/root.")
    };
    files.iter().any(|(p, _)| p.starts_with(&prefix))
}
