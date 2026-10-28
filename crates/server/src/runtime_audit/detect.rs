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

use std::path::Path;

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
    Rust,
    Next,
    React,
    Django,
    Rails,
    Laravel,
    SpringBoot,
    Express,
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
            Self::Rust => "rust",
            Self::Next => "nextjs",
            Self::React => "react",
            Self::Django => "django",
            Self::Rails => "rails",
            Self::Laravel => "laravel",
            Self::SpringBoot => "spring-boot",
            Self::Express => "express",
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
            Self::Rust => "Rust",
            Self::Next => "Next.js",
            Self::React => "React",
            Self::Django => "Django",
            Self::Rails => "Rails",
            Self::Laravel => "Laravel",
            Self::SpringBoot => "Spring Boot",
            Self::Express => "Express",
        }
    }

    /// The CPE 2.3 `part:vendor:product` NVD lists the runtime under, or `None` for a product
    /// whose vulnerabilities are not looked up here (frameworks are covered by the dependency
    /// audit through their lockfile entries; Rust is covered by RustSec through the same audit).
    pub fn cpe_prefix(self) -> Option<&'static str> {
        match self {
            Self::Node => Some("cpe:2.3:a:nodejs:node.js"),
            Self::Python => Some("cpe:2.3:a:python:python"),
            Self::Php => Some("cpe:2.3:a:php:php"),
            Self::Ruby => Some("cpe:2.3:a:ruby-lang:ruby"),
            Self::Go => Some("cpe:2.3:a:golang:go"),
            // OpenJDK vulnerabilities are listed under the oracle:openjdk product.
            Self::Java => Some("cpe:2.3:a:oracle:openjdk"),
            Self::DotNet => Some("cpe:2.3:a:microsoft:.net"),
            Self::Nginx => Some("cpe:2.3:a:f5:nginx"),
            _ => None,
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
}

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

/// Directories never walked: dependencies, build output, VCS metadata and test fixtures. Test
/// fixtures hold deliberately old versions that the repository does not run.
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
        "rust" => Some(Product::Rust),
        "nginx" => Some(Product::Nginx),
        "openjdk" | "eclipse-temurin" | "amazoncorretto" => Some(Product::Java),
        _ => None,
    }
}

/// Dockerfile `FROM` lines.
pub fn read_dockerfile(path: &str, text: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut stages: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        let mut tokens = trimmed.split_whitespace();
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
        // `FROM image AS name` registers a stage a later `FROM name` refers to.
        if rest.len() >= 3 && rest[1].eq_ignore_ascii_case("as") {
            stages.push(rest[2].to_ascii_lowercase());
        }
        if image_ref.eq_ignore_ascii_case("scratch")
            || stages.contains(&image_ref.to_ascii_lowercase())
        {
            continue;
        }
        let without_digest = image_ref.split('@').next().unwrap_or(image_ref);
        // A registry port (`host:5000/node`) is not a tag: the tag follows the last `/`.
        let (image, tag) = match without_digest.rfind(':') {
            Some(colon) if !without_digest[colon..].contains('/') => {
                (&without_digest[..colon], Some(&without_digest[colon + 1..]))
            }
            _ => (without_digest, None),
        };
        let Some(product) = docker_product(image) else {
            continue;
        };
        out.push(decl(
            product,
            path,
            i + 1,
            image_ref,
            image_tag_version(tag),
        ));
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

/// `rust-toolchain` and `rust-toolchain.toml`.
pub fn read_rust_toolchain(path: &str, text: &str) -> Vec<Declaration> {
    let (channel, needle) = if text.trim_start().starts_with('[') {
        let channel = text
            .parse::<toml::Table>()
            .ok()
            .and_then(|d| {
                d.get("toolchain")?
                    .get("channel")?
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        (channel, "channel")
    } else {
        (
            first_value_line(text).map(|(_, v)| v).unwrap_or_default(),
            "",
        )
    };
    if channel.is_empty() {
        return Vec::new();
    }
    let line = if needle.is_empty() {
        first_value_line(text).map_or(1, |(l, _)| l)
    } else {
        line_containing(text, needle, 1).unwrap_or(1)
    };
    let version = match plain_version(&channel) {
        Some(p) => version_of(p),
        None => loose("it is a moving channel, not a release"),
    };
    vec![decl(Product::Rust, path, line, &channel, version)]
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
            "rust" => (Product::Rust, value),
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

/// `pom.xml`: the `spring-boot-starter-parent` version.
pub fn read_pom(path: &str, text: &str) -> Vec<Declaration> {
    let Ok(version_tag) = Regex::new(r"<version>\s*([^<\s]+)\s*</version>") else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if !l.contains("<artifactId>spring-boot-starter-parent</artifactId>") {
            continue;
        }
        // The parent block lists groupId, artifactId and version within a few lines.
        let lo = i.saturating_sub(3);
        for (j, near) in lines.iter().enumerate().take(i + 4).skip(lo) {
            if let Some(c) = version_tag.captures(near) {
                return locked(Product::SpringBoot, path, j + 1, &c[1])
                    .into_iter()
                    .collect();
            }
        }
    }
    Vec::new()
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
        "rust-toolchain" | "rust-toolchain.toml" => read_rust_toolchain,
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
        out.declarations.extend(reader(&rel, &text));
        if out.truncated {
            return;
        }
    }
}
