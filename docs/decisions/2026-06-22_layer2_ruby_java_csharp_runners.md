# Layer-2 runners for Ruby, Java, and C# — closing the language gap

**Date:** 2026-06-22
**Status:** Accepted
**Area:** `crates/checks` (layer-2 `CheckRunner`s)

## Context

Layer-2 (the post-task `CheckRunner` gate that bounces-and-revises an agent's
draft once before commit) was polyglot but incomplete. It shipped runners for
four languages — Rust (`RustCheckRunner`), JS/TS (`JsCheckRunner`), Python
(`PythonCheckRunner`), and Go (`GoCheckRunner`). The rule corpus, however,
ships rules for **seven** languages: those four plus Ruby, Java, and C#.

The consequence was a silent coverage hole. A worktree whose only manifest was a
`Gemfile`, `pom.xml`/`build.gradle`, or `.csproj`/`.sln` was "unrecognised" by
`detect_languages`, so the selector degraded to `NoopChecks` — no layer-2
bounce-and-revise at all. Those rules rode only as agent directives plus layer-3
CI. This closes that gap so layer-2 covers the same seven languages the corpus
does.

## Decision

Add three per-language runners in `crates/checks/src/multilang.rs`, mirroring the
existing four exactly: same trait (`CheckRunner`), same coarse
`LAYER2-<LANG>-CHECKS-1` rule mapping, same **repo-pinned toolchain** and
**fail-closed** stances, same `#[cfg(test)]` binary-override seams.

### `RubyCheckRunner` — manifest `Gemfile`

- **Repo-pinned:** `Gemfile.lock` + bundler. `bundle install` materialises the
  locked gem set; every check runs through `bundle exec`, so the repo's exact
  rubocop / rspec / rake versions run (== the repo's CI).
- **What it runs:** lint via `bundle exec rubocop` (only when the repo ships a
  `.rubocop.yml`/`.rubocop.yaml`); test via `bundle exec rspec` when a `spec/`
  dir exists, else `bundle exec rake test` when a `Rakefile` exists.
- **Fail-closed:** `bundle` missing → spawn `Err`; `bundle install` fails →
  `Err`; neither a rubocop config nor a runnable test command defined → `Err`
  ("could-not-run", never a silent clean).

### `JavaCheckRunner` — manifest `pom.xml` (Maven) or `build.gradle`/`build.gradle.kts` (Gradle)

- **Repo-pinned:** prefers the repo's OWN wrapper (`./mvnw` / `./gradlew`),
  which pins the exact Maven/Gradle version; falls back to a global
  `mvn`/`gradle` only when no wrapper is present. Build tool is detected from the
  manifest (Maven takes precedence over Gradle when both exist).
- **What it runs:** Maven `verify` (`./mvnw -q verify`) or Gradle `check`
  (`./gradlew check`). These are the standard aggregate verification lifecycles;
  any checkstyle/spotbugs/test the repo binds to the build runs as part of them.
  Camerata does not bake in plugin versions.
- **Fail-closed:** no manifest → `Err`; build tool binary cannot be spawned →
  spawn `Err`; non-zero build/test exit → `LAYER2-JAVA-CHECKS-1`.

### `CSharpCheckRunner` — manifest `*.csproj` or `*.sln`

- **Repo-pinned:** the SDK is pinned by the repo's `global.json` if present;
  `dotnet` honours it automatically when invoked from the worktree. Analyzer +
  formatter rules come from the repo's project files (`.editorconfig`, package
  refs), not Camerata.
- **What it runs:** `dotnet format --verify-no-changes` (lint) +
  `dotnet build` (Roslyn analyzers) + `dotnet test`.
- **Fail-closed:** no `*.csproj`/`*.sln` → `Err`; `dotnet` cannot be spawned →
  spawn `Err`; non-zero exit on any step → `LAYER2-CSHARP-CHECKS-1`.

## Detection wiring

- `WorktreeLanguage` gains `Ruby`, `Java`, `CSharp`.
- `detect_language` (root-only, single-best-guess) and `language_for_manifest`
  (used by the recursive `detect_languages` walk) map: `Gemfile` → Ruby;
  `pom.xml`/`build.gradle`/`build.gradle.kts` → Java; `*.csproj`/`*.sln` → C#.
  C# is matched by file extension (its manifest is a glob, not a fixed name); a
  small `dir_has_extension` helper backs the root-only path.
- `PolyglotCheckRunner::from_detected` constructs the matching new runner for
  each detected `(language, dir)` pair, so a polyglot monorepo now runs the Ruby,
  Java, and C# runners alongside the others, unions their violations, and stays
  fail-closed if any sub-runner cannot run.
- Pruned-dir list gains `.gradle`, `obj`, and `bin` (Java/.NET build output) so
  nested generated manifests are not misread as separate projects.

The fleet/po-demo injection point (`runner_for_worktree`) is unchanged — it still
returns `Box<dyn CheckRunner>`; the new languages flow through automatically.

## Consequences

- Layer-2 now covers all seven languages the corpus ships rules for; the
  language gap is closed. Docs (`USER_GUIDE.md`, `TECHNICAL.md`) updated to seven
  and the "not-supported / coverage caveat" notes removed.
- Rule mapping stays intentionally coarse (one `LAYER2-<LANG>-CHECKS-1` per
  language). Fine-grained per-tool rule ids can be layered in later without
  touching the coordinator contract.
- Honesty preserved: a missing toolchain, an undefined check, or a failed dep
  install is always an `Err` ("not verified"), never a false clean.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
