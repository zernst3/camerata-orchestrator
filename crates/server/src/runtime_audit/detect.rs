//! Deterministic detection of the runtime and framework versions a repository DECLARES.
//!
//! Everything here is a pure read of files the repository already contains: a Dockerfile `FROM`
//! line, a version file, a manifest field, a lockfile entry. Each declaration records the file and
//! the line it came from, so a finding can point at the exact place to change.
//!
//! # What a declaration is, and is not
//!
//! A declaration says what the repository asks for, not what is running in production. The module
//! therefore never guesses: a version it cannot pin to a release line (`>=18`, `lts/*`, `latest`,
//! an unpinned Docker tag, a build argument) is recorded as [`Version::Loose`] with the reason, and
//! the audit reports it as not checked rather than inventing a version.
//!
//! - [`Version::Exact`]: three or more numeric parts (`18.17.0`). The release line AND the patch
//!   release are known, so end of life and known vulnerabilities can both be checked.
//! - [`Version::Release`]: a release line without a patch release (`18`, `3.11`, `^18.2.0` which
//!   stays inside the 18 line). End of life can be checked; vulnerabilities cannot, because a
//!   vulnerability is fixed in a specific patch release.
//! - [`Version::Loose`]: anything else.

use std::path::{Path, PathBuf};

use regex::Regex;

/// A runtime or framework whose end-of-life dates come from endoflife.date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Product {
    Node,
    Python,
    Php,
    Ruby,
    Go,
    Java,
    DotNet,
    Nginx,
    Next,
    React,
    Django,
    Rails,
    Laravel,
    SpringBoot,
    Express,
    /// A Dockerfile base image that is itself a build argument, so the product is unknown. It
    /// only ever appears as an excluded declaration and is never looked up.
    ContainerImage,
}

impl Product {
    /// The endoflife.date product slug (`https://endoflife.date/api/<slug>.json`).
    pub fn slug(self) -> &'static str {
        match self {
            Self::Node => "nodejs",
            Self::Python => "python",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Go => "go",
            // endoflife.date has no plain "java" product; Temurin carries the OpenJDK lines.
            Self::Java => "eclipse-temurin",
            Self::DotNet => "dotnet",
            Self::Nginx => "nginx",
            Self::Next => "nextjs",
            Self::React => "react",
            Self::Django => "django",
            Self::Rails => "rails",
            Self::Laravel => "laravel",
            Self::SpringBoot => "spring-boot",
            Self::Express => "express",
            // Never fetched: a container image of unknown product is always excluded.
            Self::ContainerImage => "container-image",
        }
    }

    /// The name a reader knows the product by.
    pub fn label(self) -> &'static str {
        match self {
            Self::Node => "Node.js",
            Self::Python => "Python",
            Self::Php => "PHP",
            Self::Ruby => "Ruby",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::DotNet => ".NET",
            Self::Nginx => "nginx",
            Self::Next => "Next.js",
            Self::React => "React",
            Self::Django => "Django",
            Self::Rails => "Rails",
            Self::Laravel => "Laravel",
            Self::SpringBoot => "Spring Boot",
            Self::Express => "Express",
            Self::ContainerImage => "Container image",
        }
    }

    /// The CPE 2.3 `part:vendor:product` names NVD lists the runtime under, or none for a product
    /// whose vulnerabilities are not looked up here (frameworks are covered by the dependency
    /// audit through their lockfile entries). nginx is listed under two vendors, the older
    /// `nginx:nginx` and the newer `f5:nginx`; both are asked and the answers merged.
    pub fn cpe_prefixes(self) -> &'static [&'static str] {
        match self {
            Self::Node => &["cpe:2.3:a:nodejs:node.js"],
            Self::Python => &["cpe:2.3:a:python:python"],
            Self::Php => &["cpe:2.3:a:php:php"],
            Self::Ruby => &["cpe:2.3:a:ruby-lang:ruby"],
            Self::Go => &["cpe:2.3:a:golang:go"],
            // OpenJDK vulnerabilities are listed under the oracle:openjdk product.
            Self::Java => &["cpe:2.3:a:oracle:openjdk"],
            Self::DotNet => &["cpe:2.3:a:microsoft:.net"],
            Self::Nginx => &["cpe:2.3:a:f5:nginx", "cpe:2.3:a:nginx:nginx"],
            _ => &[],
        }
    }
}

/// How precisely a declaration pins the version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Version {
    /// A full version, three or more numeric parts (`18.17.0`).
    Exact(String),
    /// A release line only (`18`, `3.11`).
    Release(String),
    /// Not a single release line. The text says why, in words a reader can use.
    Loose(String),
    /// A declaration that is read but deliberately not checked, with the reason in words a reader
    /// can use: a Dockerfile build stage ([`BUILD_STAGE`]) or a framework that is not a direct,
    /// non-dev dependency ([`NOT_DIRECT`]). It is listed under Coverage, never reported on.
    Excluded(String),
}

/// Why a Dockerfile stage before the final one is not checked.
pub const BUILD_STAGE: &str = "it is a build stage, and a build image does not ship";
/// Why a Dockerfile `FROM` whose whole image is a build argument is not checked.
pub const BUILD_ARG_IMAGE: &str = "the image is a build argument";
/// Why a framework found only in a lockfile is not checked.
pub const NOT_DIRECT: &str = "it is not a direct, non-dev dependency of the project";
/// Why a framework the project lists only under `optionalDependencies` is not checked.
pub const OPTIONAL_DEPENDENCY: &str = "it is only an optional dependency of the project";
/// Why a framework the project lists only under `peerDependencies` is not checked.
pub const PEER_DEPENDENCY: &str = "it is only a peer dependency of the project";

/// One version a repository declares, with where it was declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub product: Product,
    /// File path relative to the repository root, with forward slashes.
    pub path: String,
    /// 1-based line number of the declaration.
    pub line: usize,
    /// The declaration as written (`node:18-alpine`, `>=18`, `18.17.0`).
    pub raw: String,
    pub version: Version,
}

/// The result of reading one repository.
#[derive(Debug, Default)]
pub struct Detected {
    pub declarations: Vec<Declaration>,
    /// True when the walk stopped at its entry limit, so some files were not read.
    pub truncated: bool,
}

/// Directories never walked: dependencies, build output, VCS metadata, tests, examples and docs.
/// Test fixtures, examples and sample projects hold deliberately old versions that the repository
/// does not run.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    "Pods",
    "bower_components",
    "fixtures",
    "testdata",
    "test",
    "tests",
    "__tests__",
    "examples",
    "samples",
    "docs",
    "e2e",
];

/// The most directory entries the walk visits, so a pathological tree cannot stall the scan.
const WALK_ENTRY_LIMIT: usize = 50_000;

/// The largest file read (a large lockfile is a few megabytes).
const MAX_FILE_BYTES: u64 = 24 * 1024 * 1024;

// ─── version parsing ─────────────────────────────────────────────────────────

/// The constraint grammar a manifest field uses. The same text means different things in
/// different ecosystems (`^3.11` is a range of minors in Poetry and a single major line in npm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// npm `engines`: `^` keeps the major line, `~` keeps the minor line.
    Npm,
    /// Composer: `8.1.*` and `~8.1.0` pin a minor line; `^8.1` is a range.
    Composer,
    /// PEP 440: `==3.11.*`, `==3.11.4` and `~=3.11.0` pin; `>=3.9` is a range.
    Python,
    /// Gemfile: `3.2.2` and `~> 3.2.0` pin; `>= 3.0` is a range.
    Ruby,
    /// A plain version only; any operator makes it loose.
    Plain,
}

fn numeric_parts(s: &str) -> Vec<&str> {
    s.split('.').collect()
}

/// `18`, `18.17.0`, `v18`, `18.x`, `8.1.*` as a normalised numeric version (`18`, `18.17.0`,
/// `18`, `8.1`). `None` for anything else, including pre-release suffixes.
pub fn plain_version(raw: &str) -> Option<String> {
    let s = raw.trim().trim_start_matches(['v', 'V', '=']).trim();
    let s = s
        .strip_suffix(".x")
        .or_else(|| s.strip_suffix(".X"))
        .or_else(|| s.strip_suffix(".*"))
        .unwrap_or(s);
    if s.is_empty() {
        return None;
    }
    let parts = numeric_parts(s);
    if parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        Some(s.to_string())
    } else {
        None
    }
}

/// A numeric version as a [`Version`]: three or more parts are exact, fewer are a release line.
fn version_of(normalised: String) -> Version {
    if numeric_parts(&normalised).len() >= 3 {
        Version::Exact(normalised)
    } else {
        Version::Release(normalised)
    }
}

fn loose(why: &str) -> Version {
    Version::Loose(why.to_string())
}

/// Parse a version constraint as written in a manifest. Never guesses: a range, a wildcard, an
/// alias and anything unrecognised is [`Version::Loose`].
pub fn parse_constraint(raw: &str, flavor: Flavor) -> Version {
    let text = raw.trim().trim_matches(['"', '\'']).trim();
    if text.is_empty() {
        return loose("it is empty");
    }
    let lower = text.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "latest" | "stable" | "current" | "lts" | "node" | "system" | "*" | "x" | "mainline"
    ) || lower.starts_with("lts/")
    {
        return loose("it names a moving target, not a release");
    }
    // The operator, if any, then the version text after it.
    const OPERATORS: [&str; 10] = ["~>", "~=", ">=", "<=", "==", "^", "~", "=", ">", "<"];
    let (operator, rest) = OPERATORS
        .iter()
        .find_map(|op| text.strip_prefix(op).map(|r| (*op, r.trim())))
        .unwrap_or(("", text));
    if rest.contains([' ', ',', '|', '<', '>']) {
        return loose("it is a range");
    }
    let Some(version) = plain_version(rest) else {
        return loose("it is not a plain version number");
    };
    let parts = numeric_parts(&version).len();
    let wildcard = rest.ends_with(".x") || rest.ends_with(".X") || rest.ends_with(".*");
    match (flavor, operator) {
        (_, "" | "=" | "==") => {
            if wildcard {
                Version::Release(version)
            } else {
                version_of(version)
            }
        }
        (Flavor::Npm, "^") => {
            let major = version.split('.').next().unwrap_or_default().to_string();
            if major == "0" {
                loose("it is a range")
            } else {
                Version::Release(major)
            }
        }
        (Flavor::Npm, "~") => {
            let line: Vec<&str> = version.split('.').take(2).collect();
            Version::Release(line.join("."))
        }
        (Flavor::Composer, "~") | (Flavor::Python, "~=") | (Flavor::Ruby, "~>") if parts == 3 => {
            let line: Vec<&str> = version.split('.').take(2).collect();
            Version::Release(line.join("."))
        }
        (_, "~" | "~=" | "~>" | "^") => loose("it is a range"),
        (_, ">=" | "<=" | ">" | "<") => loose("it is a range"),
        _ => loose("it is not a plain version number"),
    }
}

/// An unpinned image tag or alias as a [`Version`]: `18`, `18.17.0-alpine`, `3.11-slim` yield the
/// leading numeric version; `latest`, `lts`, no tag and `${VAR}` are loose.
fn image_tag_version(tag: Option<&str>) -> Version {
    let Some(tag) = tag else {
        return loose("the image has no tag, so it follows the newest release");
    };
    if tag.contains('$') {
        return loose("it is set by a build argument");
    }
    let lead: String = tag
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let lead = lead.trim_end_matches('.');
    if lead.is_empty() {
        return loose("the tag names a moving target, not a release");
    }
    let rest = &tag[lead.len()..];
    // `8u332-jdk` (Java update releases) and `18-alpine` carry a suffix after the version.
    let rest_ok = rest.is_empty()
        || rest.starts_with('-')
        || rest.starts_with('u')
        || rest.starts_with('_')
        || rest.starts_with('.');
    if !rest_ok {
        return loose("the tag is not a plain version number");
    }
    match plain_version(lead) {
        Some(v) if rest.starts_with('u') => Version::Release(v),
        Some(v) => version_of(v),
        None => loose("the tag is not a plain version number"),
    }
}

// ─── per-file readers ────────────────────────────────────────────────────────

fn decl(product: Product, path: &str, line: usize, raw: &str, version: Version) -> Declaration {
    Declaration {
        product,
        path: path.to_string(),
        line,
        raw: raw.trim().to_string(),
        version,
    }
}

/// The 1-based line of the first line at or after `from` (a 1-based line) containing `needle`.
fn line_containing(text: &str, needle: &str, from: usize) -> Option<usize> {
    text.lines()
        .enumerate()
        .skip(from.saturating_sub(1))
        .find(|(_, l)| l.contains(needle))
        .map(|(i, _)| i + 1)
}

fn docker_product(image: &str) -> Option<Product> {
    let lower = image.to_ascii_lowercase();
    let mut segments = lower.rsplit('/');
    let name = segments.next()?;
    let parent = segments.next().unwrap_or("");
    if parent == "dotnet" && matches!(name, "sdk" | "aspnet" | "runtime" | "runtime-deps") {
        return Some(Product::DotNet);
    }
    match name {
        "node" => Some(Product::Node),
        "python" => Some(Product::Python),
        "php" => Some(Product::Php),
        "ruby" => Some(Product::Ruby),
        "golang" | "go" => Some(Product::Go),
        "nginx" => Some(Product::Nginx),
        "openjdk" | "eclipse-temurin" | "amazoncorretto" => Some(Product::Java),
        _ => None,
    }
}

/// Whether the image name itself, the part after the last `/`, is a build argument (`${BASE}`,
/// `$IMAGE`). A registry or a tag that is an argument around a literal name is not.
fn image_is_build_argument(image: &str) -> bool {
    image
        .rsplit('/')
        .next()
        .is_some_and(|name| name.contains('$'))
}

/// One `FROM` line.
struct Stage {
    line: usize,
    image_ref: String,
    /// The index of the earlier stage this one builds on (`FROM base`), when it does.
    parent: Option<usize>,
}

/// Dockerfile `FROM` lines. Only the image the FINAL stage is built on ships, so that is the only
/// declaration; a stage before it (`FROM node:16 AS build`) is a builder that never reaches the
/// image and is returned as [`Version::Excluded`]. A final stage built on an earlier stage
/// (`FROM base`) ships that stage's image, and so on down the chain.
pub fn read_dockerfile(path: &str, text: &str) -> Vec<Declaration> {
    let mut stages: Vec<Stage> = Vec::new();
    let mut aliases: Vec<(String, usize)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let mut tokens = line.split_whitespace();
        if !tokens
            .next()
            .is_some_and(|t| t.eq_ignore_ascii_case("from"))
        {
            continue;
        }
        let rest: Vec<&str> = tokens.filter(|t| !t.starts_with("--")).collect();
        let Some(image_ref) = rest.first().copied() else {
            continue;
        };
        let parent = aliases
            .iter()
            .rev()
            .find(|(alias, _)| *alias == image_ref.to_ascii_lowercase())
            .map(|(_, idx)| *idx);
        // `FROM image AS name` registers a stage a later `FROM name` refers to.
        if rest.len() >= 3 && rest[1].eq_ignore_ascii_case("as") {
            aliases.push((rest[2].to_ascii_lowercase(), stages.len()));
        }
        stages.push(Stage {
            line: i + 1,
            image_ref: image_ref.to_string(),
            parent,
        });
    }
    // The stages whose image reaches the final image: the last stage and every stage it builds on.
    let mut ships = vec![false; stages.len()];
    let mut current = stages.len().checked_sub(1);
    while let Some(idx) = current {
        ships[idx] = true;
        current = stages[idx].parent;
    }
    let mut out = Vec::new();
    for (idx, stage) in stages.iter().enumerate() {
        if stage.parent.is_some() || stage.image_ref.eq_ignore_ascii_case("scratch") {
            continue;
        }
        let without_digest = stage
            .image_ref
            .split('@')
            .next()
            .unwrap_or(&stage.image_ref);
        // A registry port (`host:5000/node`) is not a tag: the tag follows the last `/`.
        let (image, tag) = match without_digest.rfind(':') {
            Some(colon) if !without_digest[colon..].contains('/') => {
                (&without_digest[..colon], Some(&without_digest[colon + 1..]))
            }
            _ => (without_digest, None),
        };
        let Some(product) = docker_product(image) else {
            // A whole image set by a build argument (`FROM ${BASE}`) cannot be named, so it is
            // listed as not checked instead of vanishing. A stage that never ships is no gap.
            if ships[idx] && image_is_build_argument(image) {
                out.push(decl(
                    Product::ContainerImage,
                    path,
                    stage.line,
                    &stage.image_ref,
                    Version::Excluded(BUILD_ARG_IMAGE.to_string()),
                ));
            }
            continue;
        };
        let version = if ships[idx] {
            image_tag_version(tag)
        } else {
            Version::Excluded(BUILD_STAGE.to_string())
        };
        out.push(decl(product, path, stage.line, &stage.image_ref, version));
    }
    out
}

/// `.nvmrc` and `.node-version`: the first non-comment line.
pub fn read_node_version_file(path: &str, text: &str) -> Vec<Declaration> {
    first_value_line(text)
        .map(|(line, raw)| {
            vec![decl(
                Product::Node,
                path,
                line,
                &raw,
                parse_constraint(&raw, Flavor::Plain),
            )]
        })
        .unwrap_or_default()
}

fn first_value_line(text: &str) -> Option<(usize, String)> {
    text.lines().enumerate().find_map(|(i, l)| {
        let l = l.trim();
        (!l.is_empty() && !l.starts_with('#')).then(|| (i + 1, l.to_string()))
    })
}

/// `package.json` `engines.node`.
pub fn read_package_json(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(raw) = v
        .get("engines")
        .and_then(|e| e.get("node"))
        .and_then(|n| n.as_str())
    else {
        return Vec::new();
    };
    let engines_line = line_containing(text, "\"engines\"", 1).unwrap_or(1);
    let line = line_containing(text, "\"node\"", engines_line).unwrap_or(engines_line);
    vec![decl(
        Product::Node,
        path,
        line,
        raw,
        parse_constraint(raw, Flavor::Npm),
    )]
}

/// `.python-version`: one version per line (pyenv).
pub fn read_python_version_file(path: &str, text: &str) -> Vec<Declaration> {
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let l = l.trim();
            (!l.is_empty() && !l.starts_with('#')).then(|| {
                decl(
                    Product::Python,
                    path,
                    i + 1,
                    l,
                    parse_constraint(l, Flavor::Plain),
                )
            })
        })
        .collect()
}

/// `runtime.txt` (`python-3.11.4`).
pub fn read_runtime_txt(path: &str, text: &str) -> Vec<Declaration> {
    let Some((line, raw)) = first_value_line(text) else {
        return Vec::new();
    };
    match raw.strip_prefix("python-") {
        Some(version) => vec![decl(
            Product::Python,
            path,
            line,
            &raw,
            parse_constraint(version, Flavor::Plain),
        )],
        None => Vec::new(),
    }
}

/// `pyproject.toml`: `project.requires-python` and Poetry's `python` dependency.
pub fn read_pyproject(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(doc) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(raw) = doc
        .get("project")
        .and_then(|p| p.get("requires-python"))
        .and_then(|r| r.as_str())
    {
        let line = line_containing(text, "requires-python", 1).unwrap_or(1);
        out.push(decl(
            Product::Python,
            path,
            line,
            raw,
            parse_constraint(raw, Flavor::Python),
        ));
    }
    if let Some(raw) = doc
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.get("python"))
        .and_then(|r| r.as_str())
    {
        let line = line_containing(text, "python =", 1).unwrap_or(1);
        // Poetry's caret keeps a major line, which spans many Python releases: never one line.
        out.push(decl(
            Product::Python,
            path,
            line,
            raw,
            parse_constraint(raw, Flavor::Python),
        ));
    }
    out
}

/// `composer.json` `require.php`.
pub fn read_composer_json(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(raw) = v
        .get("require")
        .and_then(|r| r.get("php"))
        .and_then(|p| p.as_str())
    else {
        return Vec::new();
    };
    let require_line = line_containing(text, "\"require\"", 1).unwrap_or(1);
    let line = line_containing(text, "\"php\"", require_line).unwrap_or(require_line);
    vec![decl(
        Product::Php,
        path,
        line,
        raw,
        parse_constraint(raw, Flavor::Composer),
    )]
}

/// `.ruby-version` (`3.2.2` or `ruby-3.2.2`).
pub fn read_ruby_version_file(path: &str, text: &str) -> Vec<Declaration> {
    first_value_line(text)
        .map(|(line, raw)| {
            let version = raw.strip_prefix("ruby-").unwrap_or(&raw).to_string();
            vec![decl(
                Product::Ruby,
                path,
                line,
                &raw,
                parse_constraint(&version, Flavor::Plain),
            )]
        })
        .unwrap_or_default()
}

/// `Gemfile` `ruby '3.2.2'`.
pub fn read_gemfile(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(re) = Regex::new(r#"^\s*ruby\s+['"]([^'"]+)['"]"#) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let raw = re.captures(l)?.get(1)?.as_str().to_string();
            let version = parse_constraint(&raw, Flavor::Ruby);
            Some(decl(Product::Ruby, path, i + 1, &raw, version))
        })
        .collect()
}

/// `go.mod`: the `go` directive (a minimum language version, so only its release line counts)
/// and the `toolchain` directive (the toolchain the module asks for).
pub fn read_go_mod(path: &str, text: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let mut words = l.split_whitespace();
        match (words.next(), words.next()) {
            (Some("go"), Some(v)) => {
                if let Some(plain) = plain_version(v) {
                    // The `go` line is the minimum language version, not the toolchain in use.
                    let line_version: Vec<&str> = plain.split('.').take(2).collect();
                    out.push(decl(
                        Product::Go,
                        path,
                        i + 1,
                        v,
                        Version::Release(line_version.join(".")),
                    ));
                }
            }
            (Some("toolchain"), Some(v)) => {
                let v = v.strip_prefix("go").unwrap_or(v);
                let version = match plain_version(v) {
                    Some(p) => version_of(p),
                    None => loose("it is not a plain version number"),
                };
                out.push(decl(Product::Go, path, i + 1, v, version));
            }
            _ => {}
        }
    }
    out
}

/// `global.json` `sdk.version`. An SDK version (`8.0.100`) is the SDK's feature band, not the
/// runtime's patch release, so only its release line (`8.0`) counts.
pub fn read_global_json(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(raw) = v
        .get("sdk")
        .and_then(|s| s.get("version"))
        .and_then(|s| s.as_str())
    else {
        return Vec::new();
    };
    let line = line_containing(text, "\"version\"", 1).unwrap_or(1);
    let version = match plain_version(raw) {
        Some(p) => {
            let release: Vec<&str> = p.split('.').take(2).collect();
            Version::Release(release.join("."))
        }
        None => loose("it is not a plain version number"),
    };
    vec![decl(Product::DotNet, path, line, raw, version)]
}

/// `.csproj`, `.fsproj`, `.vbproj` and `Directory.Build.props`: `<TargetFramework(s)>`.
pub fn read_dotnet_project(path: &str, text: &str) -> Vec<Declaration> {
    let (Ok(tag), Ok(moniker)) = (
        Regex::new(r"<TargetFrameworks?>([^<]+)</TargetFrameworks?>"),
        Regex::new(r"^(?:net(\d+\.\d+)|netcoreapp(\d+\.\d+))(?:-[a-z0-9.]+)?$"),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let Some(c) = tag.captures(l) else { continue };
        for tfm in c[1].split(';') {
            let tfm = tfm.trim();
            // Only modern .NET (5 and later) and .NET Core: `net48` and `netstandard` are a
            // different product.
            let Some(m) = moniker.captures(tfm) else {
                continue;
            };
            let version = m.get(1).or_else(|| m.get(2)).map_or("", |v| v.as_str());
            out.push(decl(
                Product::DotNet,
                path,
                i + 1,
                tfm,
                Version::Release(version.to_string()),
            ));
        }
    }
    out
}

/// `.tool-versions` (asdf).
pub fn read_tool_versions(path: &str, text: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        // An asdf comment runs from a hash to the end of the line.
        let l = match l.char_indices().find(|(_, c)| *c == '\u{23}') {
            Some((at, _)) => &l[..at],
            None => l,
        }
        .trim();
        let mut words = l.split_whitespace();
        let (Some(name), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let (product, value) = match name {
            "nodejs" => (Product::Node, value),
            "python" => (Product::Python, value),
            "ruby" => (Product::Ruby, value),
            "golang" => (Product::Go, value),
            "php" => (Product::Php, value),
            "nginx" => (Product::Nginx, value),
            "dotnet" | "dotnet-core" => (Product::DotNet, value),
            // `java temurin-17.0.1` and `java openjdk-21`: the vendor prefix is not a version.
            "java" => (
                Product::Java,
                value.rsplit_once('-').map_or(value, |(_, v)| v),
            ),
            _ => continue,
        };
        out.push(decl(
            product,
            path,
            i + 1,
            value,
            parse_constraint(value, Flavor::Plain),
        ));
    }
    out
}

// ─── lockfiles: framework versions ───────────────────────────────────────────

fn locked(product: Product, path: &str, line: usize, version: &str) -> Option<Declaration> {
    let plain = plain_version(version)?;
    Some(decl(product, path, line, version, version_of(plain)))
}

/// `package-lock.json`: next, react and express.
pub fn read_package_lock(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, product) in [
        ("next", Product::Next),
        ("react", Product::React),
        ("express", Product::Express),
    ] {
        let version = v
            .get("packages")
            .and_then(|p| p.get(format!("node_modules/{name}")))
            .or_else(|| v.get("dependencies").and_then(|d| d.get(name)))
            .and_then(|e| e.get("version"))
            .and_then(|s| s.as_str());
        if let Some(version) = version {
            let key = format!("\"node_modules/{name}\"");
            let line = line_containing(text, &key, 1)
                .or_else(|| line_containing(text, &format!("\"{name}\""), 1))
                .unwrap_or(1);
            out.extend(locked(product, path, line, version));
        }
    }
    out
}

/// `yarn.lock` (classic and Berry): next, react and express.
pub fn read_yarn_lock(path: &str, text: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with(' ') || !l.trim_end().ends_with(':') {
            continue;
        }
        let header = l.trim_end().trim_end_matches(':').trim_matches('"');
        let first = header
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"');
        let name = first.rsplit_once('@').map_or(first, |(n, _)| n);
        let name = name.trim_end_matches("@npm");
        let product = match name {
            "next" => Product::Next,
            "react" => Product::React,
            "express" => Product::Express,
            _ => continue,
        };
        let version = lines.iter().skip(i + 1).take(6).find_map(|b| {
            let b = b.trim();
            b.strip_prefix("version ")
                .or_else(|| b.strip_prefix("version: "))
                .map(|v| v.trim().trim_matches('"').to_string())
        });
        if let Some(version) = version {
            out.extend(locked(product, path, i + 1, &version));
        }
    }
    out
}

/// `pnpm-lock.yaml`: next, react and express, from the package keys.
pub fn read_pnpm_lock(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(re) = Regex::new(r#"^\s*['"]?/?(next|react|express)[@/](\d+\.\d+\.\d+)"#) else {
        return Vec::new();
    };
    let mut out: Vec<Declaration> = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let Some(c) = re.captures(l) else { continue };
        let product = match &c[1] {
            "next" => Product::Next,
            "react" => Product::React,
            _ => Product::Express,
        };
        if out.iter().any(|d| d.product == product) {
            continue;
        }
        out.extend(locked(product, path, i + 1, &c[2]));
    }
    out
}

fn python_framework(name: &str) -> Option<Product> {
    (name.eq_ignore_ascii_case("django")).then_some(Product::Django)
}

/// `poetry.lock` and `uv.lock`: Django from the `[[package]]` tables.
pub fn read_python_lock(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(doc) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pkg in doc
        .get("package")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(version)) = (
            pkg.get("name").and_then(|n| n.as_str()),
            pkg.get("version").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let Some(product) = python_framework(name) else {
            continue;
        };
        let line = line_containing(text, &format!("name = \"{name}\""), 1).unwrap_or(1);
        out.extend(locked(product, path, line, version));
    }
    out
}

/// `Pipfile.lock`: Django from `default`.
pub fn read_pipfile_lock(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(version) = v
        .get("default")
        .and_then(|d| d.get("django"))
        .and_then(|d| d.get("version"))
        .and_then(|s| s.as_str())
    else {
        return Vec::new();
    };
    let line = line_containing(text, "\"django\"", 1).unwrap_or(1);
    locked(Product::Django, path, line, version.trim_start_matches('='))
        .into_iter()
        .collect()
}

/// `requirements.txt`: a pinned `Django==4.2.7`.
pub fn read_requirements(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(re) = Regex::new(r"(?i)^\s*django\s*==\s*(\d+(?:\.\d+)+)\s*(?:$|[;#\\])") else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let c = re.captures(l)?;
            locked(Product::Django, path, i + 1, &c[1])
        })
        .collect()
}

/// `Gemfile.lock`: Rails.
pub fn read_gemfile_lock(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(re) = Regex::new(r"^ {4}rails \((\d+(?:\.\d+)+)\)") else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .find_map(|(i, l)| {
            let c = re.captures(l)?;
            locked(Product::Rails, path, i + 1, &c[1])
        })
        .into_iter()
        .collect()
}

/// `composer.lock`: Laravel.
pub fn read_composer_lock(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    for key in ["packages", "packages-dev"] {
        for pkg in v.get(key).and_then(|p| p.as_array()).into_iter().flatten() {
            if pkg.get("name").and_then(|n| n.as_str()) == Some("laravel/framework") {
                if let Some(version) = pkg.get("version").and_then(|n| n.as_str()) {
                    let line = line_containing(text, "\"laravel/framework\"", 1).unwrap_or(1);
                    return locked(Product::Laravel, path, line, version)
                        .into_iter()
                        .collect();
                }
            }
        }
    }
    Vec::new()
}

/// `gradle.lockfile`: `org.springframework.boot:spring-boot:3.2.0=...`.
pub fn read_gradle_lockfile(path: &str, text: &str) -> Vec<Declaration> {
    text.lines()
        .enumerate()
        .find_map(|(i, l)| {
            let coord = l.split('=').next().unwrap_or("");
            let mut parts = coord.split(':');
            match (parts.next(), parts.next(), parts.next()) {
                (Some("org.springframework.boot"), Some("spring-boot"), Some(v)) => {
                    locked(Product::SpringBoot, path, i + 1, v)
                }
                _ => None,
            }
        })
        .into_iter()
        .collect()
}

/// `pom.xml`: the `spring-boot-starter-parent` version, read from the `<parent>` block itself. A
/// parent that takes its version from a property is not read from a neighbouring `<version>`.
pub fn read_pom(path: &str, text: &str) -> Vec<Declaration> {
    let (Ok(version_tag), Ok(artifact_tag)) = (
        Regex::new(r"<version>\s*([^<\s]+)\s*</version>"),
        Regex::new(r"<artifactId>\s*([^<\s]+)\s*</artifactId>"),
    ) else {
        return Vec::new();
    };
    let mut in_parent = false;
    let mut artifact: Option<String> = None;
    let mut version: Option<(usize, String)> = None;
    for (i, l) in text.lines().enumerate() {
        if l.contains("<parent>") {
            in_parent = true;
            artifact = None;
            version = None;
        }
        if in_parent {
            if let Some(c) = artifact_tag.captures(l) {
                artifact = Some(c[1].to_string());
            }
            if let Some(c) = version_tag.captures(l) {
                version = Some((i + 1, c[1].to_string()));
            }
        }
        if l.contains("</parent>") {
            if artifact.as_deref() == Some("spring-boot-starter-parent") {
                return version
                    .and_then(|(line, v)| locked(Product::SpringBoot, path, line, &v))
                    .into_iter()
                    .collect();
            }
            in_parent = false;
        }
    }
    Vec::new()
}

// ─── direct dependencies ─────────────────────────────────────────────────────

/// The lockfiles a framework version is read from. The project's own manifest beside them says
/// whether the framework is a direct dependency.
const FRAMEWORK_LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "Gemfile.lock",
    "composer.lock",
    "gradle.lockfile",
];

fn read_beside(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

/// How a package.json lists a package, strongest first. Only `Direct` is shipped code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NpmListing {
    /// In `dependencies`.
    Direct,
    /// Only in `optionalDependencies`.
    Optional,
    /// Only in `peerDependencies`.
    Peer,
    /// Not listed, or only in `devDependencies`.
    Absent,
}

fn npm_listing(manifest: &serde_json::Value, name: &str) -> NpmListing {
    let lists = |section: &str| manifest.get(section).and_then(|d| d.get(name)).is_some();
    if lists("dependencies") {
        NpmListing::Direct
    } else if lists("optionalDependencies") {
        NpmListing::Optional
    } else if lists("peerDependencies") {
        NpmListing::Peer
    } else {
        NpmListing::Absent
    }
}

fn read_json_beside(dir: &Path, name: &str) -> Option<serde_json::Value> {
    read_beside(dir, name).and_then(|t| serde_json::from_str(&t).ok())
}

/// The package patterns a workspace root lists: `workspaces` in its `package.json` (an array, or
/// an object with a `packages` array) and `packages` in `pnpm-workspace.yaml`.
fn workspace_patterns(dir: &Path) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    if let Some(manifest) = read_json_beside(dir, "package.json") {
        let list = manifest
            .get("workspaces")
            .and_then(|w| w.get("packages").or(Some(w)))
            .and_then(|w| w.as_array());
        patterns.extend(
            list.into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(str::to_string)),
        );
    }
    if let Some(yaml) = read_beside(dir, "pnpm-workspace.yaml") {
        let mut in_packages = false;
        for line in yaml.lines() {
            let code = line.split(" #").next().unwrap_or("").trim_end();
            if code.is_empty() {
                continue;
            }
            if !code.starts_with([' ', '\t', '-']) {
                in_packages = code.trim() == "packages:";
                continue;
            }
            if let Some(item) = code.trim().strip_prefix('-').filter(|_| in_packages) {
                patterns.push(item.trim().trim_matches(['"', '\'']).to_string());
            }
        }
    }
    patterns
}

/// Whether `name` matches `pattern`, where `*` matches any run of characters.
fn glob_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => name.strip_prefix(head).is_some_and(|rest| {
            (0..=rest.len()).any(|i| rest.is_char_boundary(i) && glob_matches(tail, &rest[i..]))
        }),
    }
}

/// The most nested folders `**` descends into.
const WORKSPACE_DEPTH: usize = 6;

/// Every folder under `base` that `segments` (a path pattern split on `/`) names.
fn expand_workspace(base: &Path, segments: &[&str], depth: usize, out: &mut Vec<PathBuf>) {
    let Some((segment, rest)) = segments.split_first() else {
        out.push(base.to_path_buf());
        return;
    };
    if depth > WORKSPACE_DEPTH {
        return;
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n != "node_modules" && !n.starts_with('.'))
        })
        .collect();
    children.sort();
    if *segment == "**" {
        // Zero folders deep, or one folder deeper with the same pattern still to match.
        expand_workspace(base, rest, depth, out);
        for child in children {
            expand_workspace(&child, segments, depth + 1, out);
        }
        return;
    }
    for child in children {
        let named = child
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| glob_matches(segment, n));
        if named {
            expand_workspace(&child, rest, depth + 1, out);
        }
    }
}

/// The folders a workspace root's patterns name, less the ones its `!` patterns remove.
fn workspace_dirs(root: &Path) -> Vec<PathBuf> {
    let mut included: Vec<PathBuf> = Vec::new();
    let mut removed: Vec<PathBuf> = Vec::new();
    for pattern in workspace_patterns(root) {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest.to_string()),
            None => (false, pattern),
        };
        let trimmed = pattern.trim_start_matches("./").trim_end_matches('/');
        let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            continue;
        }
        let target = if negated { &mut removed } else { &mut included };
        expand_workspace(root, &segments, 0, target);
    }
    included.sort();
    included.dedup();
    included.retain(|d| !removed.contains(d));
    included
}

/// How the package.json files of a project list `name`: the one beside the lockfile and, for a
/// workspace root, those of its workspace packages. The strongest listing wins.
fn npm_project_listing(dir: &Path, name: &str) -> NpmListing {
    let own =
        read_json_beside(dir, "package.json").map_or(NpmListing::Absent, |m| npm_listing(&m, name));
    if own == NpmListing::Direct {
        return own;
    }
    workspace_dirs(dir)
        .iter()
        .filter_map(|d| read_json_beside(d, "package.json"))
        .map(|m| npm_listing(&m, name))
        .fold(own, NpmListing::min)
}

/// Why `product`, found in a lockfile in `dir`, is not checked, or `None` when the manifest in the
/// same folder lists it as a direct, non-dev dependency. Anything only transitive, only a dev
/// dependency, or with no manifest beside the lockfile to show otherwise is not.
fn exclusion_reason(dir: &Path, product: Product) -> Option<&'static str> {
    if let Product::Next | Product::React | Product::Express = product {
        let name = match product {
            Product::Next => "next",
            Product::React => "react",
            _ => "express",
        };
        return match npm_project_listing(dir, name) {
            NpmListing::Direct => None,
            NpmListing::Optional => Some(OPTIONAL_DEPENDENCY),
            NpmListing::Peer => Some(PEER_DEPENDENCY),
            NpmListing::Absent => Some(NOT_DIRECT),
        };
    }
    (!is_direct_dependency(dir, product)).then_some(NOT_DIRECT)
}

/// Whether `product` is a direct, non-dev dependency declared by the manifest in `dir`, for the
/// ecosystems that have no optional or peer kind to report.
fn is_direct_dependency(dir: &Path, product: Product) -> bool {
    match product {
        Product::Laravel => read_beside(dir, "composer.json")
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .is_some_and(|v| {
                v.get("require")
                    .and_then(|d| d.get("laravel/framework"))
                    .is_some()
            }),
        Product::Rails => {
            read_beside(dir, "Gemfile").is_some_and(|t| gemfile_lists_directly(&t, "rails"))
        }
        Product::Django => python_project_lists_django(dir),
        Product::SpringBoot => ["build.gradle", "build.gradle.kts"]
            .iter()
            .filter_map(|f| read_beside(dir, f))
            .any(|t| gradle_applies_spring_boot(&t)),
        _ => true,
    }
}

/// A Gradle build file with its comments removed and its line breaks kept. A comment marker inside
/// a string (`"https://example.com"`) is not a comment; a string ends at its closing quote or at
/// the end of its line.
fn strip_gradle_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == q || c == '\n' {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                while chars.peek().is_some_and(|n| *n != '\n') {
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                    }
                    if previous == '*' && n == '/' {
                        break;
                    }
                    previous = n;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Whether a Gradle build file (Groovy or Kotlin DSL) puts Spring Boot into the shipped
/// application: an `implementation`, `api`, `compile` or `runtimeOnly` dependency on an
/// `org.springframework.boot` artifact (a `platform(...)` import counts), or the
/// `org.springframework.boot` plugin applied. A test or compile-only configuration, a plugin
/// declared `apply false`, a comment and a string that only mentions the name do not count.
fn gradle_applies_spring_boot(text: &str) -> bool {
    const START: &str = r"(?m)(?:^|[{;])[ \t]*";
    const CONFIGURATION: &str = r"(?:implementation|api|compile|runtimeOnly)\b[ \t]*\(?[ \t]*";
    const BOOT: &str = r#"['"]org\.springframework\.boot"#;
    let code = strip_gradle_comments(text);
    let dependency_forms = [
        // implementation 'org.springframework.boot:spring-boot-starter'
        format!(r"{START}{CONFIGURATION}(?:(?:enforcedPlatform|platform)[ \t]*\([ \t]*)?{BOOT}:"),
        // implementation group: 'org.springframework.boot', name: '...'
        format!(r#"{START}{CONFIGURATION}group[ \t]*[:=][ \t]*{BOOT}['"]"#),
        // apply plugin: 'org.springframework.boot'
        format!(r"{START}apply[ \t]*\(?[ \t]*plugin[ \t]*[:=][ \t]*{BOOT}['\x22]"),
    ];
    if dependency_forms
        .iter()
        .any(|pattern| Regex::new(pattern).is_ok_and(|re| re.is_match(&code)))
    {
        return true;
    }
    // id 'org.springframework.boot' version '3.2.0', but not when it ends `apply false`.
    let (Ok(plugin), Ok(not_applied)) = (
        Regex::new(&format!(
            r#"{START}id[ \t]*\(?[ \t]*{BOOT}['"][ \t]*\)?([^\n;}}]*)"#
        )),
        Regex::new(r"\bapply\s+false\b"),
    ) else {
        return false;
    };
    let applied = plugin
        .captures_iter(&code)
        .any(|c| !not_applied.is_match(&c[1]));
    applied
}

/// The group names in a Gemfile fragment (`:development, :test`), and whether all of them are
/// development-only groups.
fn only_dev_groups(fragment: &str) -> bool {
    let Ok(symbol) = Regex::new(r":([A-Za-z_]+)") else {
        return false;
    };
    let names: Vec<String> = symbol
        .captures_iter(fragment)
        .map(|c| c[1].to_string())
        .collect();
    !names.is_empty() && names.iter().all(|n| n == "development" || n == "test")
}

/// Whether a Gemfile lists `gem` outside every development or test group.
fn gemfile_lists_directly(text: &str, gem: &str) -> bool {
    let Ok(gem_line) = Regex::new(&format!(r#"^\s*gem\s+['"]{gem}['"](.*)$"#)) else {
        return false;
    };
    // One entry per open `do` block: true for a development or test group.
    let mut blocks: Vec<bool> = Vec::new();
    for line in text.lines() {
        let code = line.trim();
        if code.starts_with('\u{23}') {
            continue;
        }
        let opens_block = code.ends_with(" do") || code.contains(" do |") || code == "do";
        if opens_block {
            let group = code.starts_with("group ") || code.starts_with("group(");
            blocks.push(group && only_dev_groups(code));
        } else if code == "end" {
            blocks.pop();
        } else if let Some(c) = gem_line.captures(line) {
            let inline = &c[1];
            let inline_dev = inline.contains("group") && only_dev_groups(inline);
            if !blocks.iter().any(|dev| *dev) && !inline_dev {
                return true;
            }
        }
    }
    false
}

/// The distribution name at the start of a requirement string (`Django>=4.2` is `django`).
fn requirement_name(requirement: &str) -> String {
    requirement
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect::<String>()
        .to_ascii_lowercase()
        .replace(['_', '.'], "-")
}

/// Whether the project file beside a Python lockfile lists Django as a direct, non-dev
/// dependency: `project.dependencies` or `tool.poetry.dependencies` in `pyproject.toml`, or
/// `[packages]` in a `Pipfile`.
fn python_project_lists_django(dir: &Path) -> bool {
    let in_pyproject = read_beside(dir, "pyproject.toml")
        .and_then(|t| t.parse::<toml::Table>().ok())
        .is_some_and(|doc| {
            let pep621 = doc
                .get("project")
                .and_then(|p| p.get("dependencies"))
                .and_then(|d| d.as_array())
                .is_some_and(|deps| {
                    deps.iter()
                        .filter_map(|d| d.as_str())
                        .any(|d| requirement_name(d) == "django")
                });
            let poetry = doc
                .get("tool")
                .and_then(|t| t.get("poetry"))
                .and_then(|p| p.get("dependencies"))
                .and_then(|d| d.as_table())
                .is_some_and(|deps| deps.keys().any(|k| requirement_name(k) == "django"));
            pep621 || poetry
        });
    in_pyproject
        || read_beside(dir, "Pipfile")
            .and_then(|t| t.parse::<toml::Table>().ok())
            .and_then(|doc| doc.get("packages").and_then(|p| p.as_table()).cloned())
            .is_some_and(|pkgs| pkgs.keys().any(|k| requirement_name(k) == "django"))
}

// ─── the walk ────────────────────────────────────────────────────────────────

type Reader = fn(&str, &str) -> Vec<Declaration>;

/// The reader for a file name, or `None` when the file declares nothing this module reads.
fn reader_for(name: &str) -> Option<Reader> {
    let lower = name.to_ascii_lowercase();
    if lower == "dockerfile" || lower.starts_with("dockerfile.") || lower.ends_with(".dockerfile") {
        return Some(read_dockerfile);
    }
    if lower.ends_with(".csproj")
        || lower.ends_with(".fsproj")
        || lower.ends_with(".vbproj")
        || lower == "directory.build.props"
    {
        return Some(read_dotnet_project);
    }
    Some(match name {
        ".nvmrc" | ".node-version" => read_node_version_file,
        "package.json" => read_package_json,
        ".python-version" => read_python_version_file,
        "runtime.txt" => read_runtime_txt,
        "pyproject.toml" => read_pyproject,
        "composer.json" => read_composer_json,
        ".ruby-version" => read_ruby_version_file,
        "Gemfile" => read_gemfile,
        "go.mod" => read_go_mod,
        "global.json" => read_global_json,
        ".tool-versions" => read_tool_versions,
        "package-lock.json" | "npm-shrinkwrap.json" => read_package_lock,
        "yarn.lock" => read_yarn_lock,
        "pnpm-lock.yaml" => read_pnpm_lock,
        "poetry.lock" | "uv.lock" => read_python_lock,
        "Pipfile.lock" => read_pipfile_lock,
        "requirements.txt" => read_requirements,
        "Gemfile.lock" => read_gemfile_lock,
        "composer.lock" => read_composer_lock,
        "gradle.lockfile" => read_gradle_lockfile,
        "pom.xml" => read_pom,
        _ => return None,
    })
}

/// Read every declaration under `root`. Files and directories are visited in sorted order so the
/// result is the same on every run.
pub fn detect_declarations(root: &Path) -> Detected {
    let mut detected = Detected::default();
    let mut visited = 0usize;
    walk(root, root, &mut visited, &mut detected);
    detected
}

fn walk(root: &Path, dir: &Path, visited: &mut usize, out: &mut Detected) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        *visited += 1;
        if *visited > WALK_ENTRY_LIMIT {
            out.truncated = true;
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(root, &path, visited, out);
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let Some(reader) = reader_for(&name) else {
            continue;
        };
        if entry.metadata().map_or(true, |m| m.len() > MAX_FILE_BYTES) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let mut found = reader(&rel, &text);
        if FRAMEWORK_LOCKFILES.contains(&name.as_str()) {
            let folder = path.parent().unwrap_or(root);
            for d in &mut found {
                if let Some(why) = exclusion_reason(folder, d.product) {
                    d.version = Version::Excluded(why.to_string());
                }
            }
        }
        out.declarations.extend(found);
        if out.truncated {
            return;
        }
    }
}
