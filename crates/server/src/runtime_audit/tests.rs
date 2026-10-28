//! Network-free tests for the runtime end-of-life and runtime vulnerability pass.
//!
//! Every feed answer comes from the recorded fixtures under
//! `crates/server/tests/fixtures/runtime_audit/` (real endoflife.date and NVD responses, reduced
//! to the fields the audit reads) through the `FeedFetcher` seam. No test touches the network.

use std::sync::Mutex;

use chrono::{DateTime, TimeZone, Utc};

use super::detect::{self, Flavor, Product, Version};
use super::eol::{standing, CycleMatch, EolDate, EolFeed, Standing};
use super::*;
use crate::vuln_intel::{FetchStatusError, KEV_URL};

const EOL_NODE: &str = include_str!("../../tests/fixtures/runtime_audit/endoflife_nodejs.json");
const EOL_PYTHON: &str = include_str!("../../tests/fixtures/runtime_audit/endoflife_python.json");
const EOL_DOTNET: &str = include_str!("../../tests/fixtures/runtime_audit/endoflife_dotnet.json");
const EOL_DJANGO: &str = include_str!("../../tests/fixtures/runtime_audit/endoflife_django.json");
const NVD_NODE_18_17_0: &str =
    include_str!("../../tests/fixtures/runtime_audit/nvd_nodejs_18.17.0.json");
const KEV_FIXTURE_RAW: &str = include_str!("../../tests/fixtures/vuln_intel/kev.json");

const EPSS_RUNTIME: &str = r#"{"status":"OK","data":[
  {"cve":"CVE-2023-44487","epss":"0.944010000","percentile":"0.999","date":"2026-10-27"}]}"#;

// ─── fixtures ─────────────────────────────────────────────────────────────────

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap()
}

/// The real KEV fixture plus CVE-2023-44487 (the HTTP/2 rapid reset, which really is on the
/// catalog) and filler rows to clear the parser's sanity floor.
fn kev_json() -> String {
    let mut doc: serde_json::Value = serde_json::from_str(KEV_FIXTURE_RAW).unwrap();
    let rows = doc["vulnerabilities"].as_array_mut().unwrap();
    rows.push(serde_json::json!({"cveID": "CVE-2023-44487", "dateAdded": "2023-10-10"}));
    for i in 0..crate::vuln_intel::KEV_MIN_ENTRIES {
        rows.push(
            serde_json::json!({"cveID": format!("CVE-1999-{i:05}"), "dateAdded": "2000-01-01"}),
        );
    }
    serde_json::to_string(&doc).unwrap()
}

type Handler = Box<dyn Fn(&str, usize) -> anyhow::Result<String> + Send + Sync>;

/// A fetcher driven by a closure over `(url, how many times that exact url was asked before)`.
struct Fixture {
    handler: Handler,
    calls: Mutex<Vec<String>>,
}

impl Fixture {
    fn new(
        handler: impl Fn(&str, usize) -> anyhow::Result<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Every feed answers from its recorded fixture.
    fn online() -> Self {
        Self::new(|url, _| online_answer(url))
    }

    fn offline() -> Self {
        Self::new(|url, _| Err(anyhow::anyhow!("fixture: {url} unreachable")))
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn calls_to(&self, needle: &str) -> usize {
        self.calls().iter().filter(|u| u.contains(needle)).count()
    }
}

fn online_answer(url: &str) -> anyhow::Result<String> {
    let body = if url == super::eol::product_url("nodejs") {
        EOL_NODE.to_string()
    } else if url == super::eol::product_url("python") {
        EOL_PYTHON.to_string()
    } else if url == super::eol::product_url("dotnet") {
        EOL_DOTNET.to_string()
    } else if url == super::eol::product_url("django") {
        EOL_DJANGO.to_string()
    } else if url.starts_with(nvd::NVD_API) && url.contains("nodejs:node.js:18.17.0:") {
        NVD_NODE_18_17_0.to_string()
    } else if url == KEV_URL {
        kev_json()
    } else if url.starts_with(crate::vuln_intel::EPSS_URL) {
        EPSS_RUNTIME.to_string()
    } else {
        anyhow::bail!("fixture: no recorded answer for {url}");
    };
    Ok(body)
}

#[async_trait::async_trait]
impl FeedFetcher for Fixture {
    async fn get(&self, url: &str) -> anyhow::Result<String> {
        let nth = {
            let mut calls = self.calls.lock().unwrap();
            let nth = calls.iter().filter(|u| u.as_str() == url).count();
            calls.push(url.to_string());
            nth
        };
        (self.handler)(url, nth)
    }
}

fn rate_limited(retry_after_secs: Option<u64>) -> anyhow::Error {
    FetchStatusError {
        status: 429,
        retry_after_secs,
    }
    .into()
}

/// A repository folder holding the given files.
fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    dir
}

async fn run(
    dir: &tempfile::TempDir,
    fetcher: &Fixture,
    cache: Option<&std::path::Path>,
) -> RuntimeAudit {
    audit_runtimes(
        &[("acme/app".to_string(), dir.path().to_path_buf())],
        fetcher,
        cache,
        now(),
        &NvdPolicy::instant(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await
}

fn eol_findings(a: &RuntimeAudit) -> Vec<&Finding> {
    a.findings
        .iter()
        .filter(|f| f.rule_id == EOL_RULE_ID)
        .collect()
}

fn vuln_findings(a: &RuntimeAudit) -> Vec<&Finding> {
    a.findings
        .iter()
        .filter(|f| f.rule_id == VULN_RULE_ID)
        .collect()
}

fn notes_text(a: &RuntimeAudit) -> String {
    a.notes
        .iter()
        .map(|n| n.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// ─── version detection, per source ───────────────────────────────────────────

fn only(decls: Vec<Declaration>) -> Declaration {
    assert_eq!(decls.len(), 1, "{decls:?}");
    decls.into_iter().next().unwrap()
}

#[test]
fn a_dockerfile_from_line_gives_the_image_tag_and_its_line() {
    let text = "# build\nFROM node:16-alpine AS build\nRUN npm ci\nFROM python:3.11.4-slim\n";
    let d = detect::read_dockerfile("Dockerfile", text);
    assert_eq!(d.len(), 2);
    assert_eq!(d[0].product, Product::Node);
    assert_eq!(d[0].line, 2);
    assert_eq!(d[0].raw, "node:16-alpine");
    assert_eq!(d[0].version, Version::Release("16".to_string()));
    assert_eq!(d[1].product, Product::Python);
    assert_eq!(d[1].line, 4);
    assert_eq!(d[1].version, Version::Exact("3.11.4".to_string()));
}

#[test]
fn a_dockerfile_floating_tag_or_build_argument_is_loose_never_guessed() {
    for (from, want) in [
        ("FROM node:latest", "moving target"),
        ("FROM node", "no tag"),
        ("FROM node:lts-alpine", "moving target"),
        ("FROM node:${NODE_VERSION}-alpine", "build argument"),
        ("FROM nginx:stable", "moving target"),
    ] {
        let d = only(detect::read_dockerfile("Dockerfile", from));
        match d.version {
            Version::Loose(why) => assert!(why.contains(want), "{from}: {why}"),
            other => panic!("{from}: expected a loose version, got {other:?}"),
        }
    }
}

#[test]
fn a_dockerfile_skips_scratch_stage_references_and_unrelated_images() {
    let text = "FROM --platform=$BUILDPLATFORM node:20 AS build\nFROM build AS test\nFROM scratch\nFROM ubuntu:22.04\nFROM registry.example.com:5000/node:18.17.0\n";
    let d = detect::read_dockerfile("Dockerfile", text);
    let lines: Vec<usize> = d.iter().map(|x| x.line).collect();
    assert_eq!(lines, vec![1, 5], "{d:?}");
    assert_eq!(d[1].version, Version::Exact("18.17.0".to_string()));
}

#[test]
fn a_dockerfile_reads_dotnet_java_and_nginx_images() {
    let text = "FROM mcr.microsoft.com/dotnet/aspnet:8.0\nFROM openjdk:17-jdk-slim\nFROM eclipse-temurin:8u332-jdk\nFROM nginx:1.25.3-alpine\n";
    let d = detect::read_dockerfile("Dockerfile", text);
    let got: Vec<(Product, Version)> = d.into_iter().map(|x| (x.product, x.version)).collect();
    assert_eq!(
        got,
        vec![
            (Product::DotNet, Version::Release("8.0".to_string())),
            (Product::Java, Version::Release("17".to_string())),
            (Product::Java, Version::Release("8".to_string())),
            (Product::Nginx, Version::Exact("1.25.3".to_string())),
        ]
    );
}

#[test]
fn node_version_files_read_a_pin_and_flag_an_alias() {
    assert_eq!(
        only(detect::read_node_version_file(".nvmrc", "v18.17.0\n")).version,
        Version::Exact("18.17.0".to_string())
    );
    assert_eq!(
        only(detect::read_node_version_file(
            ".node-version",
            "# pinned\n20\n"
        ))
        .line,
        2
    );
    for alias in ["lts/*", "lts/hydrogen", "node", "latest"] {
        assert!(
            matches!(
                only(detect::read_node_version_file(".nvmrc", alias)).version,
                Version::Loose(_)
            ),
            "{alias}"
        );
    }
}

#[test]
fn package_json_engines_node_is_a_range_unless_it_pins_a_line() {
    let at = |engines: &str| {
        let text = format!(
            "{{\n  \"name\": \"x\",\n  \"engines\": {{\n    \"node\": \"{engines}\"\n  }}\n}}\n"
        );
        only(detect::read_package_json("package.json", &text))
    };
    let range = at(">=18");
    assert_eq!(range.line, 4);
    assert_eq!(range.raw, ">=18");
    assert!(matches!(range.version, Version::Loose(ref w) if w.contains("range")));
    assert!(matches!(at("^18 || ^20").version, Version::Loose(_)));
    assert!(matches!(at("*").version, Version::Loose(_)));
    // Inside one release line: the line is known, the patch is not.
    assert_eq!(at("18.x").version, Version::Release("18".to_string()));
    assert_eq!(at("^18.2.0").version, Version::Release("18".to_string()));
    assert_eq!(
        at("~18.17.1").version,
        Version::Release("18.17".to_string())
    );
    assert_eq!(at("18.17.1").version, Version::Exact("18.17.1".to_string()));
    assert!(detect::read_package_json("package.json", "{\"name\": \"x\"}").is_empty());
}

#[test]
fn python_sources_runtime_txt_python_version_and_pyproject() {
    assert_eq!(
        only(detect::read_python_version_file(
            ".python-version",
            "3.11.4\n"
        ))
        .version,
        Version::Exact("3.11.4".to_string())
    );
    assert!(matches!(
        only(detect::read_python_version_file(
            ".python-version",
            "system\n"
        ))
        .version,
        Version::Loose(_)
    ));
    assert_eq!(
        only(detect::read_runtime_txt("runtime.txt", "python-3.10.12\n")).version,
        Version::Exact("3.10.12".to_string())
    );
    let pyproject = "[project]\nname = \"x\"\nrequires-python = \">=3.9\"\n";
    let d = only(detect::read_pyproject("pyproject.toml", pyproject));
    assert_eq!(d.line, 3);
    assert!(matches!(d.version, Version::Loose(_)));
    let pinned = "[project]\nrequires-python = \"==3.11.*\"\n";
    assert_eq!(
        only(detect::read_pyproject("pyproject.toml", pinned)).version,
        Version::Release("3.11".to_string())
    );
    let poetry = "[tool.poetry.dependencies]\npython = \"^3.11\"\n";
    assert!(matches!(
        only(detect::read_pyproject("pyproject.toml", poetry)).version,
        Version::Loose(_)
    ));
}

#[test]
fn composer_json_require_php_is_a_range_unless_it_pins_a_line() {
    let at = |php: &str| {
        let text = format!("{{\n  \"require\": {{\n    \"php\": \"{php}\"\n  }}\n}}\n");
        only(detect::read_composer_json("composer.json", &text))
    };
    let caret = at("^8.1");
    assert_eq!(caret.line, 3);
    assert!(matches!(caret.version, Version::Loose(_)));
    assert!(matches!(at(">=8.0").version, Version::Loose(_)));
    assert_eq!(at("8.1.*").version, Version::Release("8.1".to_string()));
    assert_eq!(at("~8.1.0").version, Version::Release("8.1".to_string()));
    assert_eq!(at("8.1.5").version, Version::Exact("8.1.5".to_string()));
}

#[test]
fn ruby_sources_ruby_version_and_gemfile() {
    assert_eq!(
        only(detect::read_ruby_version_file(
            ".ruby-version",
            "ruby-3.2.2\n"
        ))
        .version,
        Version::Exact("3.2.2".to_string())
    );
    let gemfile = "source 'https://rubygems.org'\nruby '~> 3.2.0'\ngem 'rails'\n";
    let d = only(detect::read_gemfile("Gemfile", gemfile));
    assert_eq!(d.line, 2);
    assert_eq!(d.version, Version::Release("3.2".to_string()));
    assert!(matches!(
        only(detect::read_gemfile("Gemfile", "ruby '>= 3.0'\n")).version,
        Version::Loose(_)
    ));
}

#[test]
fn go_mod_go_line_is_a_minimum_so_only_its_release_line_counts() {
    let text = "module x\n\ngo 1.21.4\n\ntoolchain go1.22.1\n";
    let d = detect::read_go_mod("go.mod", text);
    assert_eq!(d.len(), 2);
    assert_eq!(d[0].line, 3);
    assert_eq!(d[0].version, Version::Release("1.21".to_string()));
    assert_eq!(d[1].line, 5);
    assert_eq!(d[1].version, Version::Exact("1.22.1".to_string()));
}

#[test]
fn rust_toolchain_reads_a_release_and_flags_a_channel() {
    let toml = "[toolchain]\nchannel = \"1.75.0\"\ncomponents = [\"clippy\"]\n";
    let d = only(detect::read_rust_toolchain("rust-toolchain.toml", toml));
    assert_eq!(d.line, 2);
    assert_eq!(d.version, Version::Exact("1.75.0".to_string()));
    assert!(matches!(
        only(detect::read_rust_toolchain(
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"stable\"\n"
        ))
        .version,
        Version::Loose(_)
    ));
    assert!(matches!(
        only(detect::read_rust_toolchain(
            "rust-toolchain",
            "nightly-2024-01-01\n"
        ))
        .version,
        Version::Loose(_)
    ));
}

#[test]
fn dotnet_sources_global_json_and_target_frameworks() {
    let global = "{\n  \"sdk\": {\n    \"version\": \"8.0.100\"\n  }\n}\n";
    let d = only(detect::read_global_json("global.json", global));
    assert_eq!(d.line, 3);
    // 100 is the SDK feature band, not a runtime patch release.
    assert_eq!(d.version, Version::Release("8.0".to_string()));
    let csproj = "<Project>\n  <PropertyGroup>\n    <TargetFrameworks>net6.0;net8.0-windows;net48;netstandard2.0;netcoreapp3.1</TargetFrameworks>\n  </PropertyGroup>\n</Project>\n";
    let found: Vec<(usize, Version)> = detect::read_dotnet_project("App.csproj", csproj)
        .into_iter()
        .map(|d| (d.line, d.version))
        .collect();
    assert_eq!(
        found,
        vec![
            (3, Version::Release("6.0".to_string())),
            (3, Version::Release("8.0".to_string())),
            (3, Version::Release("3.1".to_string())),
        ]
    );
}

#[test]
fn tool_versions_reads_each_known_tool() {
    let text = "# asdf\nnodejs 18.17.0\npython 3.11.4 3.10.12\nruby 3.2.2\ngolang 1.21.4\njava temurin-17.0.1\ndotnet system\nterraform 1.5.0\n";
    let got: Vec<(Product, usize, Version)> = detect::read_tool_versions(".tool-versions", text)
        .into_iter()
        .map(|d| (d.product, d.line, d.version))
        .collect();
    assert_eq!(got.len(), 6, "{got:?}");
    assert_eq!(
        got[0],
        (Product::Node, 2, Version::Exact("18.17.0".to_string()))
    );
    assert_eq!(
        got[1],
        (Product::Python, 3, Version::Exact("3.11.4".to_string()))
    );
    assert_eq!(
        got[4],
        (Product::Java, 6, Version::Exact("17.0.1".to_string()))
    );
    assert!(matches!(got[5].2, Version::Loose(_)));
}

#[test]
fn lockfiles_give_framework_versions_with_their_lines() {
    let package_lock = "{\n  \"lockfileVersion\": 3,\n  \"packages\": {\n    \"node_modules/next\": {\n      \"version\": \"14.1.0\"\n    },\n    \"node_modules/react\": {\n      \"version\": \"18.2.0\"\n    },\n    \"node_modules/a/node_modules/express\": {\n      \"version\": \"3.0.0\"\n    }\n  }\n}\n";
    let d = detect::read_package_lock("package-lock.json", package_lock);
    let got: Vec<(Product, usize, Version)> = d
        .into_iter()
        .map(|d| (d.product, d.line, d.version))
        .collect();
    assert_eq!(
        got,
        vec![
            (Product::Next, 4, Version::Exact("14.1.0".to_string())),
            (Product::React, 7, Version::Exact("18.2.0".to_string())),
        ],
        "a nested copy of express is not the project's express"
    );

    let yarn = "# yarn lockfile v1\n\nexpress@^4.17.1, express@^4.18.0:\n  version \"4.18.2\"\n  resolved \"x\"\n";
    let d = only(detect::read_yarn_lock("yarn.lock", yarn));
    assert_eq!((d.product, d.line), (Product::Express, 3));
    assert_eq!(d.version, Version::Exact("4.18.2".to_string()));

    let pnpm = "packages:\n\n  /next@13.5.6(react@18.2.0):\n    resolution: {}\n";
    let d = only(detect::read_pnpm_lock("pnpm-lock.yaml", pnpm));
    assert_eq!((d.product, d.line), (Product::Next, 3));

    let poetry = "[[package]]\nname = \"asgiref\"\nversion = \"3.7.2\"\n\n[[package]]\nname = \"django\"\nversion = \"4.2.7\"\n";
    let d = only(detect::read_python_lock("poetry.lock", poetry));
    assert_eq!((d.product, d.line), (Product::Django, 6));
    assert_eq!(d.version, Version::Exact("4.2.7".to_string()));

    let gem = "GEM\n  specs:\n    actionpack (7.1.2)\n    rails (7.1.2)\n";
    let d = only(detect::read_gemfile_lock("Gemfile.lock", gem));
    assert_eq!((d.product, d.line), (Product::Rails, 4));

    let composer = "{\n  \"packages\": [\n    {\n      \"name\": \"laravel/framework\",\n      \"version\": \"v10.38.0\"\n    }\n  ]\n}\n";
    let d = only(detect::read_composer_lock("composer.lock", composer));
    assert_eq!((d.product, d.line), (Product::Laravel, 4));
    assert_eq!(d.version, Version::Exact("10.38.0".to_string()));

    let gradle =
        "# lock\norg.springframework.boot:spring-boot:3.2.0=compileClasspath,runtimeClasspath\n";
    let d = only(detect::read_gradle_lockfile("gradle.lockfile", gradle));
    assert_eq!((d.product, d.line), (Product::SpringBoot, 2));

    let pom = "<project>\n  <parent>\n    <groupId>org.springframework.boot</groupId>\n    <artifactId>spring-boot-starter-parent</artifactId>\n    <version>2.7.18</version>\n  </parent>\n</project>\n";
    let d = only(detect::read_pom("pom.xml", pom));
    assert_eq!((d.product, d.line), (Product::SpringBoot, 5));
    assert_eq!(d.version, Version::Exact("2.7.18".to_string()));

    let reqs = "requests==2.31.0\nDjango==3.2.25  # pinned\n";
    let d = only(detect::read_requirements("requirements.txt", reqs));
    assert_eq!((d.product, d.line), (Product::Django, 2));
}

#[test]
fn the_walk_reads_nested_files_skips_vendored_and_fixture_folders_and_is_stable() {
    let dir = repo(&[
        ("Dockerfile", "FROM node:16\n"),
        ("services/api/.nvmrc", "18.17.0\n"),
        ("node_modules/dep/Dockerfile", "FROM node:10\n"),
        ("tests/fixtures/Dockerfile", "FROM node:8\n"),
        ("README.md", "FROM node:6\n"),
    ]);
    let first = detect::detect_declarations(dir.path());
    let places: Vec<(&str, usize)> = first
        .declarations
        .iter()
        .map(|d| (d.path.as_str(), d.line))
        .collect();
    assert_eq!(places, vec![("Dockerfile", 1), ("services/api/.nvmrc", 1)]);
    assert!(!first.truncated);
    assert_eq!(
        detect::detect_declarations(dir.path()).declarations,
        first.declarations
    );
}

#[test]
fn constraint_grammar_never_turns_a_range_into_a_pin() {
    for raw in [
        ">=18",
        "<20",
        "^18 || ^20",
        ">=18 <20",
        "18 - 20",
        "*",
        "x",
        "latest",
        "",
        "lts/*",
    ] {
        assert!(
            matches!(
                detect::parse_constraint(raw, Flavor::Npm),
                Version::Loose(_)
            ),
            "{raw:?}"
        );
    }
    // Operators that mean different things per ecosystem stay loose where the span is wider than
    // one release line.
    assert!(matches!(
        detect::parse_constraint("^3.11", Flavor::Python),
        Version::Loose(_)
    ));
    assert!(matches!(
        detect::parse_constraint("~=3.11", Flavor::Python),
        Version::Loose(_)
    ));
    assert_eq!(
        detect::parse_constraint("~=3.11.0", Flavor::Python),
        Version::Release("3.11".to_string())
    );
}

// ─── end of life ─────────────────────────────────────────────────────────────

#[test]
fn the_feed_parses_dates_booleans_and_refuses_a_body_it_cannot_believe() {
    let feed = EolFeed::parse(r#"[{"cycle":"2","eol":"2020-01-01"},{"cycle":"3","eol":true},{"cycle":"4","eol":false},{"cycle":"5","eol":"not a date"},{"nope":1}]"#).unwrap();
    let got: Vec<(&str, &EolDate)> = feed
        .cycles
        .iter()
        .map(|c| (c.cycle.as_str(), &c.eol))
        .collect();
    assert_eq!(
        got,
        vec![
            ("2", &EolDate::On("2020-01-01".to_string())),
            ("3", &EolDate::Ended),
            ("4", &EolDate::Supported),
        ]
    );
    assert!(EolFeed::parse("{}").is_err());
    assert!(EolFeed::parse("[]").is_err());
    assert!(EolFeed::parse(r#"[{"cycle":"1"}]"#).is_err());
    assert!(EolFeed::parse("<html>").is_err());
}

#[test]
fn a_declared_version_maps_to_the_longest_matching_release_line() {
    let python = EolFeed::parse(EOL_PYTHON).unwrap();
    assert!(matches!(python.cycle_for("3.11.4"), CycleMatch::Found(c) if c.cycle == "3.11"));
    assert!(matches!(python.cycle_for("3.1"), CycleMatch::Unlisted));
    // `3` is every 3.x line at once.
    assert!(matches!(python.cycle_for("3"), CycleMatch::TooBroad));
    let dotnet = EolFeed::parse(EOL_DOTNET).unwrap();
    assert!(matches!(dotnet.cycle_for("8.0"), CycleMatch::Found(c) if c.cycle == "8"));
    assert!(matches!(dotnet.cycle_for("1.2"), CycleMatch::Unlisted));
}

#[test]
fn end_of_life_is_past_near_or_future_by_the_day() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
    let on = |d: &str| EolDate::On(d.to_string());
    assert_eq!(
        standing(&on("2023-09-11"), today, 90),
        Standing::Ended {
            since: Some("2023-09-11".to_string())
        }
    );
    assert_eq!(
        standing(&on("2026-10-27"), today, 90),
        Standing::Ended {
            since: Some("2026-10-27".to_string())
        }
    );
    // The last day of support is not yet past.
    assert_eq!(
        standing(&on("2026-10-28"), today, 90),
        Standing::EndsSoon {
            on: "2026-10-28".to_string(),
            days: 0
        }
    );
    assert_eq!(
        standing(&on("2027-01-26"), today, 90),
        Standing::EndsSoon {
            on: "2027-01-26".to_string(),
            days: 90
        }
    );
    assert_eq!(standing(&on("2027-01-27"), today, 90), Standing::Supported);
    assert_eq!(
        standing(&EolDate::Ended, today, 90),
        Standing::Ended { since: None }
    );
    assert_eq!(
        standing(&EolDate::Supported, today, 90),
        Standing::Supported
    );
}

#[tokio::test]
async fn a_runtime_past_its_end_of_life_is_a_high_finding_in_the_stated_words() {
    let dir = repo(&[("Dockerfile", "FROM node:16-alpine\n")]);
    let audit = run(&dir, &Fixture::online(), None).await;
    let findings = eol_findings(&audit);
    assert_eq!(findings.len(), 1, "{:?}", audit.findings);
    let f = findings[0];
    assert_eq!(f.severity, "high");
    assert_eq!((f.path.as_str(), f.line), ("Dockerfile", 1));
    assert_eq!(f.repo, "acme/app");
    assert!(
        f.detail
            .starts_with("Node.js 16 has received no security fixes since 2023-09-11."),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("`node:16-alpine` in Dockerfile line 1"),
        "{}",
        f.detail
    );
    // The nearest line that is still supported above 16 on this day is 22 (18 and 20 have ended).
    assert!(f.detail.contains("Node.js 22"), "{}", f.detail);
    assert_eq!(f.captures[RUNTIME_CAPTURE], "Node.js 16");
    assert_eq!(f.captures[EOL_DATE_CAPTURE], "2023-09-11");
    assert!(f
        .fix_specific
        .as_deref()
        .is_some_and(|s| s.contains("Node.js 22")));
    assert_eq!(f.kind, Some(camerata_api_types::audit::FindingKind::Defect));
}

#[tokio::test]
async fn an_end_of_life_within_ninety_days_is_a_low_advisory() {
    let csproj = "<Project>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n";
    let dir = repo(&[("src/App.csproj", csproj)]);
    let audit = run(&dir, &Fixture::online(), None).await;
    let findings = eol_findings(&audit);
    assert_eq!(findings.len(), 1, "{:?}", audit.findings);
    let f = findings[0];
    assert_eq!(f.severity, "low");
    assert!(
        f.detail
            .starts_with("Advisory: .NET 8 reaches its end of life on 2026-11-10, in 13 days"),
        "{}",
        f.detail
    );
    assert_eq!((f.path.as_str(), f.line), ("src/App.csproj", 3));
}

#[tokio::test]
async fn a_supported_release_line_gives_no_finding_and_no_claim() {
    let poetry = "[[package]]\nname = \"django\"\nversion = \"5.2.1\"\n";
    let dir = repo(&[("poetry.lock", poetry)]);
    let audit = run(&dir, &Fixture::online(), None).await;
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
    // Nothing was wrong with the lookup, so nothing is disclosed as unchecked.
    assert!(audit.notes.is_empty(), "{:?}", audit.notes);
}

#[tokio::test]
async fn one_release_line_declared_in_several_files_is_one_finding_naming_each_place() {
    let dir = repo(&[
        ("Dockerfile", "FROM node:16\n"),
        (".nvmrc", "16\n"),
        (
            "package.json",
            "{\n  \"engines\": {\n    \"node\": \"16.x\"\n  }\n}\n",
        ),
    ]);
    let audit = run(&dir, &Fixture::online(), None).await;
    let findings = eol_findings(&audit);
    assert_eq!(findings.len(), 1, "{:?}", audit.findings);
    let f = findings[0];
    assert_eq!((f.path.as_str(), f.line), (".nvmrc", 1));
    let mut also: Vec<(&str, usize)> = f
        .also_locations
        .iter()
        .map(|l| (l.path.as_str(), l.line))
        .collect();
    also.sort();
    assert_eq!(also, vec![("Dockerfile", 1), ("package.json", 3)]);
    assert!(
        f.detail
            .contains("also declared in Dockerfile line 1 and package.json line 3"),
        "{}",
        f.detail
    );
}

#[tokio::test]
async fn a_framework_lockfile_entry_past_its_end_of_life_is_found() {
    let poetry = "[[package]]\nname = \"django\"\nversion = \"3.2.25\"\n";
    let dir = repo(&[("poetry.lock", poetry)]);
    let audit = run(&dir, &Fixture::online(), None).await;
    let findings = eol_findings(&audit);
    assert_eq!(findings.len(), 1, "{:?} {:?}", audit.findings, audit.notes);
    assert!(
        findings[0]
            .detail
            .starts_with("Django 3.2 has received no security fixes since"),
        "{}",
        findings[0].detail
    );
    assert_eq!(
        (findings[0].path.as_str(), findings[0].line),
        ("poetry.lock", 2)
    );
}

#[tokio::test]
async fn a_loose_version_is_listed_as_not_checked_and_never_guessed() {
    let dir = repo(&[
        (
            "package.json",
            "{\n  \"engines\": {\n    \"node\": \">=18\"\n  }\n}\n",
        ),
        ("Dockerfile", "FROM node:latest\n"),
    ]);
    let fetcher = Fixture::online();
    let audit = run(&dir, &fetcher, None).await;
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
    let text = notes_text(&audit);
    assert!(
        text.contains("Not checked, because the declared version is not a single release"),
        "{text}"
    );
    assert!(text.contains("`>=18` in package.json line 3"), "{text}");
    assert!(
        text.contains("`node:latest` in Dockerfile line 1"),
        "{text}"
    );
    // Nothing was looked up for a version that was never determined.
    assert_eq!(fetcher.calls().len(), 0, "{:?}", fetcher.calls());
    assert!(matches!(audit.eol, FeedStamp::NotChecked { .. }));
}

#[tokio::test]
async fn a_release_line_too_broad_or_unlisted_is_disclosed_not_guessed() {
    let dir = repo(&[
        (".python-version", "3\n"),
        ("global.json", "{\"sdk\": {\"version\": \"1.2.300\"}}"),
    ]);
    let audit = run(&dir, &Fixture::online(), None).await;
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
    let text = notes_text(&audit);
    assert!(text.contains("names several release lines"), "{text}");
    assert!(text.contains("does not list the release line"), "{text}");
}

// ─── the end-of-life feed fails ──────────────────────────────────────────────

#[tokio::test]
async fn an_unreachable_end_of_life_feed_is_a_coverage_line_and_never_a_clean_claim() {
    let dir = repo(&[("Dockerfile", "FROM node:16\n")]);
    let audit = run(&dir, &Fixture::offline(), None).await;
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
    let text = notes_text(&audit);
    assert!(
        text.to_lowercase()
            .contains("runtime end-of-life data unavailable in this scan"),
        "{text}"
    );
    assert!(text.contains("Node.js"), "{text}");
    assert!(
        matches!(audit.eol, FeedStamp::NotChecked { .. }),
        "{:?}",
        audit.eol
    );
    assert!(
        audit.eol.text().starts_with("not checked"),
        "{}",
        audit.eol.text()
    );
    // The disclosure line the report prints is built from these notes.
    let lines = crate::report_export::tool_coverage_disclosures(&audit.notes);
    // One line for the end-of-life feed and one for the vulnerability check that a release line
    // without a patch version could not have.
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Runtime end-of-life data unavailable in this scan")),
        "{lines:?}"
    );
    for line in &lines {
        assert!(!line.contains('\u{2014}'), "no em dash: {line}");
        assert!(
            !line.to_lowercase().contains("no end-of-life"),
            "no clean claim: {line}"
        );
    }
}

#[tokio::test]
async fn a_cached_end_of_life_copy_stands_in_when_the_live_fetch_fails_and_says_so() {
    let dir = repo(&[("Dockerfile", "FROM node:16\n")]);
    let cache = tempfile::tempdir().unwrap();
    // A good run writes the cache.
    let warm = run(&dir, &Fixture::online(), Some(cache.path())).await;
    assert_eq!(eol_findings(&warm).len(), 1);
    assert_eq!(
        warm.eol,
        FeedStamp::Checked {
            as_of: "2026-10-28".to_string(),
            note: None
        }
    );
    // Three days later the feed is down: the cache is used, dated, and the stamp says so.
    let later = audit_runtimes(
        &[("acme/app".to_string(), dir.path().to_path_buf())],
        &Fixture::offline(),
        Some(cache.path()),
        now() + chrono::Duration::days(3),
        &NvdPolicy::instant(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(eol_findings(&later).len(), 1, "{:?}", later.notes);
    match &later.eol {
        FeedStamp::Checked { as_of, note } => {
            assert_eq!(as_of, "2026-10-28");
            assert!(
                note.as_deref()
                    .is_some_and(|n| n.contains("cached copy fetched 2026-10-28 UTC")),
                "{note:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    // Eight days later the cache is too old to use.
    let stale = audit_runtimes(
        &[("acme/app".to_string(), dir.path().to_path_buf())],
        &Fixture::offline(),
        Some(cache.path()),
        now() + chrono::Duration::days(8),
        &NvdPolicy::instant(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await;
    assert!(stale.findings.is_empty());
    assert!(matches!(stale.eol, FeedStamp::NotChecked { .. }));
    assert!(notes_text(&stale).contains("end-of-life data unavailable"));
}

#[tokio::test]
async fn one_product_failing_does_not_hide_the_others() {
    let dir = repo(&[("Dockerfile", "FROM node:16\nFROM php:7.4-fpm\n")]);
    let audit = run(&dir, &Fixture::online(), None).await;
    // Node answered; PHP has no recorded answer and counts as unavailable.
    assert_eq!(eol_findings(&audit).len(), 1);
    let text = notes_text(&audit);
    assert!(text.contains("could not be reached for PHP"), "{text}");
    match &audit.eol {
        FeedStamp::Checked { note, .. } => {
            assert!(
                note.as_deref()
                    .is_some_and(|n| n.contains("not checked for PHP")),
                "{note:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

// ─── runtime vulnerabilities ─────────────────────────────────────────────────

#[test]
fn the_cpe_name_is_built_only_for_a_product_and_a_plain_version() {
    assert_eq!(
        nvd::cpe_name(Product::Node, "18.17.0").as_deref(),
        Some("cpe:2.3:a:nodejs:node.js:18.17.0:*:*:*:*:*:*:*")
    );
    assert_eq!(
        nvd::cpe_name(Product::Django, "4.2.7"),
        None,
        "frameworks are the dependency audit's"
    );
    assert_eq!(nvd::cpe_name(Product::Node, "18.x"), None);
    assert_eq!(nvd::cpe_name(Product::Node, ""), None);
}

#[tokio::test]
async fn a_runtime_with_a_patch_version_gets_one_finding_rated_by_its_highest_cvss() {
    let dir = repo(&[("Dockerfile", "FROM node:18.17.0-alpine\n")]);
    let fetcher = Fixture::online();
    let audit = run(&dir, &fetcher, None).await;
    let findings = vuln_findings(&audit);
    assert_eq!(findings.len(), 1, "{:?} {:?}", audit.findings, audit.notes);
    let f = findings[0];
    // CVE-2023-32006 is CVSS 8.8, the highest of the three; the dependency audit's scorer maps it.
    assert_eq!(f.severity, "high");
    assert_eq!((f.path.as_str(), f.line), ("Dockerfile", 1));
    assert!(
        f.detail
            .starts_with("Node.js 18.17.0 has 3 known vulnerabilities"),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("CVE-2023-32006 (CVSS 8.8, high)"),
        "{}",
        f.detail
    );
    assert_eq!(
        f.captures[CVE_IDS_CAPTURE],
        "CVE-2023-32006,CVE-2023-44487,CVE-2026-21711"
    );
    // The lookup carried only a product name and a version.
    let nvd_calls: Vec<String> = fetcher
        .calls()
        .into_iter()
        .filter(|u| u.starts_with(nvd::NVD_API))
        .collect();
    assert_eq!(nvd_calls.len(), 1);
    assert!(
        nvd_calls[0].contains("cpeName=cpe:2.3:a:nodejs:node.js:18.17.0:*:*:*:*:*:*:*"),
        "{nvd_calls:?}"
    );
    assert_eq!(
        audit.nvd,
        FeedStamp::Checked {
            as_of: "2026-10-28".to_string(),
            note: None
        }
    );
    // 18.17.0 is also past its end of life, as its own finding under its own rule.
    assert_eq!(eol_findings(&audit).len(), 1);
}

#[tokio::test]
async fn a_release_line_without_a_patch_version_is_not_looked_up_and_says_so() {
    let dir = repo(&[("Dockerfile", "FROM node:18\n")]);
    let fetcher = Fixture::online();
    let audit = run(&dir, &fetcher, None).await;
    assert!(vuln_findings(&audit).is_empty());
    assert_eq!(fetcher.calls_to(nvd::NVD_API), 0);
    let text = notes_text(&audit);
    assert!(text.contains("Known vulnerabilities were not checked, because the declared version has no patch release"), "{text}");
    assert!(text.contains("`node:18` in Dockerfile line 1"), "{text}");
    assert!(matches!(audit.nvd, FeedStamp::NotChecked { .. }));
}

#[tokio::test]
async fn a_known_exploited_runtime_cve_raises_the_finding_to_critical_with_epss() {
    let dir = repo(&[("Dockerfile", "FROM node:18.17.0\n")]);
    let fetcher = Fixture::online();
    let audit = run(&dir, &fetcher, None).await;
    let mut findings = audit.findings.clone();
    let osv = vec![crate::vuln_intel::OsvRun {
        repo: "acme/app".to_string(),
        ran: true,
    }];
    let (stamp, _notes) =
        crate::vuln_intel::enrich_with(&mut findings, &osv, &fetcher, None, now()).await;
    let f = findings.iter().find(|f| f.rule_id == VULN_RULE_ID).unwrap();
    assert_eq!(f.severity, "critical");
    assert!(
        f.detail
            .contains("Known exploited: CVE-2023-44487 is listed in the CISA KEV catalog"),
        "{}",
        f.detail
    );
    assert!(
        f.detail.contains("Severity raised from high to critical."),
        "{}",
        f.detail
    );
    assert!(
        f.detail
            .contains("EPSS (CVE-2023-44487): 94.40% probability"),
        "{}",
        f.detail
    );
    assert_eq!(
        f.captures[crate::vuln_intel::KNOWN_EXPLOITED_CAPTURE],
        "CVE-2023-44487"
    );
    // The end-of-life finding carries no CVE and is left alone.
    let eol = findings.iter().find(|f| f.rule_id == EOL_RULE_ID).unwrap();
    assert_eq!(eol.severity, "high");
    assert!(!eol.detail.contains("Known exploited"));
    assert!(matches!(stamp.cisa_kev, FeedStamp::Checked { .. }));
}

#[tokio::test]
async fn kev_still_enriches_a_runtime_when_the_dependency_audit_did_not_run() {
    let dir = repo(&[("Dockerfile", "FROM node:18.17.0\n")]);
    let fetcher = Fixture::online();
    let audit = run(&dir, &fetcher, None).await;
    let mut findings = audit.findings.clone();
    let osv = vec![crate::vuln_intel::OsvRun {
        repo: "acme/app".to_string(),
        ran: false,
    }];
    let (stamp, _) =
        crate::vuln_intel::enrich_with(&mut findings, &osv, &fetcher, None, now()).await;
    assert!(findings.iter().any(|f| f.severity == "critical"));
    assert!(
        matches!(stamp.osv, FeedStamp::NotChecked { .. }),
        "the dependency audit still did not run"
    );
    assert!(matches!(stamp.cisa_kev, FeedStamp::Checked { .. }));
}

#[tokio::test]
async fn nvd_rate_limiting_is_retried_with_backoff_and_then_succeeds() {
    let dir = repo(&[("Dockerfile", "FROM node:18.17.0\n")]);
    let fetcher = Fixture::new(|url, nth| {
        if url.starts_with(nvd::NVD_API) && nth < 2 {
            return Err(rate_limited(if nth == 0 { Some(1) } else { None }));
        }
        online_answer(url)
    });
    let audit = run(&dir, &fetcher, None).await;
    assert_eq!(vuln_findings(&audit).len(), 1, "{:?}", audit.notes);
    assert_eq!(
        fetcher.calls_to(nvd::NVD_API),
        3,
        "two refusals then the answer"
    );
}

#[tokio::test]
async fn nvd_rate_limiting_that_never_clears_never_fails_the_scan() {
    let dir = repo(&[("Dockerfile", "FROM node:16.20.2\nFROM python:3.9.18\n")]);
    let fetcher = Fixture::new(|url, _| {
        if url.starts_with(nvd::NVD_API) {
            return Err(rate_limited(None));
        }
        online_answer(url)
    });
    let audit = run(&dir, &fetcher, None).await;
    // The end-of-life findings are intact; only the vulnerability data is missing, and said so.
    assert_eq!(eol_findings(&audit).len(), 2, "{:?}", audit.findings);
    assert!(vuln_findings(&audit).is_empty());
    let text = notes_text(&audit);
    assert!(
        text.to_lowercase()
            .contains("runtime vulnerability data unavailable in this scan"),
        "{text}"
    );
    assert!(matches!(audit.nvd, FeedStamp::NotChecked { .. }));
    // One lookup, three attempts (the first and two retries); the second runtime was not hammered.
    assert_eq!(fetcher.calls_to(nvd::NVD_API), 3, "{:?}", fetcher.calls());
}

#[tokio::test]
async fn an_unreachable_nvd_is_a_coverage_line_and_never_a_clean_claim() {
    let dir = repo(&[(".nvmrc", "18.17.0\n")]);
    let fetcher = Fixture::new(|url, _| {
        if url.starts_with(nvd::NVD_API) {
            anyhow::bail!("fixture: NVD unreachable");
        }
        online_answer(url)
    });
    let audit = run(&dir, &fetcher, None).await;
    assert!(vuln_findings(&audit).is_empty());
    let lines = crate::report_export::tool_coverage_disclosures(&audit.notes);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("Runtime vulnerability data unavailable in this scan"),
        "{lines:?}"
    );
    assert!(lines[0].contains("Node.js 18.17.0"), "{lines:?}");
    assert!(
        !lines[0].to_lowercase().contains("no known vulnerabilities"),
        "{lines:?}"
    );
    assert_eq!(
        audit.nvd.text(),
        "not checked (the feed could not be fetched)"
    );
}

#[tokio::test]
async fn a_cached_nvd_answer_stands_in_for_a_failed_lookup() {
    let dir = repo(&[(".nvmrc", "18.17.0\n")]);
    let cache = tempfile::tempdir().unwrap();
    let warm = run(&dir, &Fixture::online(), Some(cache.path())).await;
    assert_eq!(vuln_findings(&warm).len(), 1);
    let later = audit_runtimes(
        &[("acme/app".to_string(), dir.path().to_path_buf())],
        &Fixture::offline(),
        Some(cache.path()),
        now() + chrono::Duration::days(2),
        &NvdPolicy::instant(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(vuln_findings(&later).len(), 1);
    assert!(
        matches!(&later.nvd, FeedStamp::Checked { note: Some(n), .. } if n.contains("cached copy fetched 2026-10-28 UTC")),
        "{:?}",
        later.nvd
    );
}

#[tokio::test]
async fn nvd_pages_are_read_until_the_total_is_reached() {
    let page = |ids: &[&str], total: usize, start: usize| {
        let rows: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| serde_json::json!({"cve": {"id": id, "metrics": {"cvssMetricV31": [{"type": "Primary", "cvssData": {"vectorString": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H", "baseScore": 9.8}}]}}}))
            .collect();
        serde_json::json!({"totalResults": total, "startIndex": start, "timestamp": "2026-10-28T01:00:00.000", "vulnerabilities": rows}).to_string()
    };
    let first = page(&["CVE-2024-0001", "CVE-2024-0002"], 3, 0);
    let second = page(&["CVE-2024-0003"], 3, 2);
    let fetcher = Fixture::new(move |url, _| {
        if url.contains("startIndex=0") {
            Ok(first.clone())
        } else if url.contains("startIndex=2") {
            Ok(second.clone())
        } else {
            anyhow::bail!("unexpected {url}")
        }
    });
    let result = nvd::lookup(
        &fetcher,
        "cpe:2.3:a:nodejs:node.js:1.0.0:*:*:*:*:*:*:*",
        &NvdPolicy::instant(),
    )
    .await
    .unwrap();
    let ids: Vec<&str> = result.cves.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["CVE-2024-0001", "CVE-2024-0002", "CVE-2024-0003"]);
    assert!(result
        .cves
        .iter()
        .all(|c| c.severity == "critical" && c.score == Some(9.8)));
    assert_eq!(result.as_of.as_deref(), Some("2026-10-28"));
    assert_eq!(fetcher.calls().len(), 2);
}

#[tokio::test]
async fn cves_without_a_rating_are_held_for_review_never_rated_silently() {
    let body = serde_json::json!({
        "totalResults": 1,
        "timestamp": "2026-10-28T01:00:00.000",
        "vulnerabilities": [{"cve": {"id": "CVE-2026-99999", "vulnStatus": "Awaiting Analysis"}}]
    })
    .to_string();
    let dir = repo(&[(".nvmrc", "18.17.0\n")]);
    let fetcher = Fixture::new(move |url, _| {
        if url.starts_with(nvd::NVD_API) {
            Ok(body.clone())
        } else {
            online_answer(url)
        }
    });
    let audit = run(&dir, &fetcher, None).await;
    let f = vuln_findings(&audit)[0];
    assert_eq!(f.severity, "medium");
    assert!(f.needs_review);
    assert!(
        f.detail.contains("Severity unknown, held for review"),
        "{}",
        f.detail
    );
}

#[tokio::test]
async fn a_runtime_with_no_known_vulnerabilities_makes_no_finding_and_no_claim() {
    let body = serde_json::json!({"totalResults": 0, "timestamp": "2026-10-28T01:00:00.000", "vulnerabilities": []}).to_string();
    let dir = repo(&[(".python-version", "3.12.1\n")]);
    let fetcher = Fixture::new(move |url, _| {
        if url.starts_with(nvd::NVD_API) {
            Ok(body.clone())
        } else {
            online_answer(url)
        }
    });
    let audit = run(&dir, &fetcher, None).await;
    assert!(audit.findings.is_empty());
    assert!(audit.notes.is_empty());
    assert_eq!(
        audit.nvd,
        FeedStamp::Checked {
            as_of: "2026-10-28".to_string(),
            note: None
        }
    );
}

// ─── the provenance stamp ────────────────────────────────────────────────────

#[tokio::test]
async fn the_stamp_dates_both_feeds_exactly_like_kev_and_epss() {
    let dir = repo(&[("Dockerfile", "FROM node:18.17.0\n")]);
    let audit = run(&dir, &Fixture::online(), None).await;
    let stamp = VulnDataStamp {
        endoflife: audit.eol.clone(),
        nvd: audit.nvd.clone(),
        ..VulnDataStamp::all_not_checked("x")
    };
    let lines = crate::vuln_intel::readme_lines(Some(&stamp));
    assert!(
        lines.contains("Vulnerability data (End-of-life, endoflife.date) as of 2026-10-28\n"),
        "{lines}"
    );
    assert!(
        lines.contains("Vulnerability data (NVD, runtimes) as of 2026-10-28\n"),
        "{lines}"
    );
    let manifest = crate::vuln_intel::manifest_json(Some(&stamp));
    assert_eq!(manifest["end_of_life"]["status"], "checked");
    assert_eq!(manifest["end_of_life"]["as_of"], "2026-10-28");
    assert_eq!(
        manifest["nvd"]["stamp"],
        "Vulnerability data (NVD, runtimes) as of 2026-10-28"
    );
    // The README line and the manifest cannot disagree: both come from the same entries.
    for (key, _, s) in crate::vuln_intel::stamp_entries(Some(&stamp)) {
        assert!(manifest[key]["stamp"]
            .as_str()
            .unwrap()
            .ends_with(&s.text()));
    }
}

#[test]
fn a_stamp_from_before_the_two_feeds_existed_reads_as_not_checked() {
    let old = r#"{"cisa_kev":{"status":"checked","as_of":"2026-10-20"},
        "first_epss":{"status":"not_checked","reason":"r"},
        "osv":{"status":"checked","as_of":"2026-10-20"}}"#;
    let stamp: VulnDataStamp = serde_json::from_str(old).unwrap();
    assert!(matches!(stamp.endoflife, FeedStamp::NotChecked { .. }));
    assert!(matches!(stamp.nvd, FeedStamp::NotChecked { .. }));
    let lines = crate::vuln_intel::readme_lines(Some(&stamp));
    assert_eq!(lines.lines().count(), 5, "{lines}");
}

// ─── wiring: the report, the stamp carry-over and the disabled switch ────────

fn empty_report() -> ScanReport {
    let mut report: ScanReport = serde_json::from_value(serde_json::json!({
        "repos": ["acme/app"], "stacks": [], "files_scanned": 0, "test_file_count": 0,
        "files_excluded": 0, "code_chars": 0, "code_lines": 0,
        "code_lines_by_language": [], "excluded_mechanical_rules": [], "findings": [],
        "proposed_rules": [], "gated": false, "blocked": false, "coverage_notes": [],
        "provenance": {
            "audited_refs": [], "mode": "parallel", "thorough": false, "deep": false,
            "rules_fingerprint": "", "audited_rule_ids": [], "camerata_version": "t",
            "started_at": "", "finished_at": "", "model_sessions": []
        }
    }))
    .unwrap();
    report.findings.clear();
    report
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_disabled_switch_reads_nothing_and_fetches_nothing() {
    let _env = camerata_test_env::EnvGuard::set(crate::dep_audit::DISABLE_ENV_VAR, "1");
    let dir = repo(&[("Dockerfile", "FROM node:16\n")]);
    let mut report = empty_report();
    run_runtime_audit(
        &mut report,
        &[("acme/app".to_string(), dir.path().to_path_buf())],
    )
    .await;
    assert!(report.findings.is_empty());
    let stamp = report.provenance.vulnerability_data.clone().unwrap();
    assert!(
        stamp.endoflife.text().contains("disabled"),
        "{}",
        stamp.endoflife.text()
    );
    assert!(stamp.nvd.text().contains("disabled"));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_enrichment_keeps_the_runtime_pass_stamps() {
    let _env = camerata_test_env::EnvGuard::set(crate::dep_audit::DISABLE_ENV_VAR, "1");
    let mut report = empty_report();
    let mut stamp = VulnDataStamp::all_not_checked("x");
    stamp.endoflife = FeedStamp::Checked {
        as_of: "2026-10-28".to_string(),
        note: None,
    };
    stamp.nvd = FeedStamp::Checked {
        as_of: "2026-10-27".to_string(),
        note: None,
    };
    report.provenance.vulnerability_data = Some(stamp);
    crate::vuln_intel::enrich_dependency_findings(&mut report, 0, &[]).await;
    let after = report.provenance.vulnerability_data.unwrap();
    assert_eq!(after.endoflife.text(), "as of 2026-10-28");
    assert_eq!(after.nvd.text(), "as of 2026-10-27");
}

#[tokio::test]
async fn the_findings_are_stable_across_runs() {
    let dir = repo(&[
        ("Dockerfile", "FROM node:16\nFROM python:3.9.18\n"),
        (
            "src/App.csproj",
            "<TargetFramework>net8.0</TargetFramework>\n",
        ),
    ]);
    let a = run(&dir, &Fixture::online(), None).await;
    let b = run(&dir, &Fixture::online(), None).await;
    assert_eq!(a.findings, b.findings);
    assert!(a.findings.len() >= 3);
}

#[test]
fn no_client_text_the_pass_writes_has_an_em_dash_or_a_serial_comma() {
    let text = [
        join_names(&["a".to_string(), "b".to_string(), "c".to_string()]),
        join_capped(&(0..9).map(|i| i.to_string()).collect::<Vec<_>>()),
        REASON_NOTHING_DECLARED.to_string(),
        REASON_NO_EXACT_RUNTIME.to_string(),
    ]
    .join("\n");
    assert!(!text.contains('\u{2014}'));
    assert!(!text.contains(", and "));
    assert_eq!(
        join_names(&["a".to_string(), "b".to_string(), "c".to_string()]),
        "a, b and c"
    );
}
