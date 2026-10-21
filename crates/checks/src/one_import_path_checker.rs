//! `OneImportPathChecker`: the deterministic detector for `ARCH-ONE-IMPORT-PATH-1` ("a symbol
//! has one import path"). The algorithm and its false-positive traps are specified in
//! `docs/design/2026-10-21_w29-detector-implementation.md` ("ARCH-ONE-IMPORT-PATH-1: the
//! canonical-symbol algorithm and its traps"); this module implements that version, not the
//! older "symbol to paths" idea.
//!
//! **Not registered yet.** Registration in [`crate::arch_checker::all_checkers`] and the
//! corpus rule (`javascript` domain) land together once the owner approves the rule.
//!
//! # How it decides
//!
//! 1. Build each ECMA file's export surface ([`crate::extract::exports`]): its own names,
//!    its named re-exports (origin AND exported name) and its star re-exports. Specifiers are
//!    resolved with [`EcmaResolver`], the same resolution the import graph uses.
//! 2. Canonicalize `(file, name)` through the re-export edges to the `(origin_file,
//!    origin_name)` that really defines it. Own exports win over star re-exports; a star never
//!    carries `default`; two star sources that both supply the name, a re-export cycle, or an
//!    opaque (unresolvable) star source make the name uncertain, and an uncertain name is
//!    skipped, never guessed.
//! 3. Every named or default import becomes a use of a canonical symbol, reached through a
//!    resolved target file. Group by canonical symbol; the distinct targets are the "paths".
//! 4. Drop importers that are test files, and importers inside the directory subtree of any
//!    module that re-exports the symbol (a barrel's own internals import their siblings
//!    directly by convention). Flag when two or more distinct targets remain.
//!
//! Keying the group on the canonical symbol is what catches the rule's real case (the same
//! symbol imported from its origin AND from a barrel: two different resolved files), while
//! keying the paths on RESOLVED targets keeps `./utils` versus `../a/utils`, or a tsconfig
//! alias versus a relative path to the same file, from ever counting as two paths.
//!
//! # Documented limits (all false-negative, by design)
//!
//! - A re-exporter at the repo root (directory `""`) has the whole repo as its subtree, so it
//!   excludes every importer and the symbol is never flagged.
//! - `import * as ns`, side-effect imports, dynamic `import()` / `require()` and re-export
//!   statements are not importers (which members `ns` uses is unknown without member-access
//!   analysis).
//! - `import { a } from './x'; export { a };` is read as an own export of the re-exporting
//!   file (the extractor does not track local bindings), so that indirect barrel form is not
//!   followed.
//! - A re-export no one imports both ways is not flagged: there is no verifiable split.
//!
//! # Severity
//!
//! Always [`SEVERITY_MEDIUM`]: the checker contract forbids emitting `low`. The corpus rule
//! declares `max_severity = "low"`, which the report export clamps down to (see
//! `report_export::clamp_to_rule_max_severity`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_MEDIUM};
use crate::extract::resolver::EcmaResolver;
use crate::extract::{self, ImportKind, SourceLang};

pub const RULE_ONE_IMPORT_PATH: &str = "ARCH-ONE-IMPORT-PATH-1";

const RULE_IDS: &[&str] = &[RULE_ONE_IMPORT_PATH];

/// The ECMA source files the export extractor reads, plus `tsconfig.json` (the resolver reads
/// `compilerOptions.paths` aliases from it, and the Layer-2 runner only loads files matching
/// the armed checkers' interest globs, so the alias config must be declared here too).
const INTEREST_GLOBS: &[&str] = &[
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.mjs",
    "**/*.cjs",
    "tsconfig.json",
    "**/tsconfig.json",
];

/// How many importers a finding names per path before saying "and N more".
const IMPORTERS_SHOWN_PER_PATH: usize = 3;

/// Re-export chains deeper than this are treated as uncertain (a runaway guard on top of the
/// cycle guard; real barrels nest two or three deep).
const MAX_CHAIN_DEPTH: usize = 32;

pub struct OneImportPathChecker;

impl ArchChecker for OneImportPathChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let resolver = EcmaResolver::new(repo.files);
        let model = Model::build(repo.files, &resolver);
        let usages = model.collect_usages(repo.files, &resolver);

        let mut out: Vec<ArchViolation> = usages
            .into_iter()
            .filter_map(|((origin_file, name), group)| judge_group(&origin_file, &name, &group))
            .collect();
        out.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.message.as_str()).cmp(&(
                b.file.as_str(),
                b.line,
                b.message.as_str(),
            ))
        });
        out
    }
}

// ─── the re-export model ────────────────────────────────────────────────────────────────

/// One re-exporting statement on a symbol's chain: the file that holds it and its line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Hop {
    file: String,
    line: usize,
}

/// A name's canonical identity, with the re-export statements it was reached through
/// (outermost first; empty when the name was reached at its origin).
#[derive(Debug, Clone)]
struct Found {
    origin_file: String,
    origin_name: String,
    hops: Vec<Hop>,
}

#[derive(Debug, Clone)]
enum Canon {
    Found(Found),
    /// The module does not (provably) export the name.
    Absent,
    /// The name cannot be pinned to one origin: a cycle, an ambiguous star, a duplicate
    /// export, an opaque star source or an over-deep chain. Skipped, never guessed.
    Uncertain,
}

/// A named re-export entry: `target` is `None` when the specifier does not resolve to a repo
/// file (an external package), in which case the name is exported but its origin is unknown.
struct NamedEdge {
    target: Option<String>,
    origin: String,
    line: usize,
}

#[derive(Default)]
struct Module {
    own: BTreeSet<String>,
    named: BTreeMap<String, Vec<NamedEdge>>,
    /// `(resolved target or None when unresolvable, line)`.
    stars: Vec<(Option<String>, usize)>,
}

type Memo = HashMap<(String, String), Canon>;

struct Model {
    modules: HashMap<String, Module>,
}

impl Model {
    fn build(files: &[(String, String)], resolver: &EcmaResolver<'_>) -> Self {
        let mut modules = HashMap::new();
        for (path, content) in files {
            let Some(lang) = ecma_lang(path) else {
                continue;
            };
            let surface = extract::exports(lang, content);
            let mut m = Module {
                own: surface.own.into_iter().collect(),
                ..Module::default()
            };
            for re in surface.named_re_exports {
                m.named.entry(re.exported).or_default().push(NamedEdge {
                    target: resolver.resolve(path, &re.specifier),
                    origin: re.origin,
                    line: re.line,
                });
            }
            for star in surface.star_re_exports {
                m.stars
                    .push((resolver.resolve(path, &star.specifier), star.line));
            }
            modules.insert(path.clone(), m);
        }
        Self { modules }
    }

    /// Canonicalize `(file, name)` to the file and name that really define it.
    fn canonicalize(
        &self,
        file: &str,
        name: &str,
        path: &mut Vec<(String, String)>,
        memo: &mut Memo,
    ) -> Canon {
        let key = (file.to_string(), name.to_string());
        if let Some(hit) = memo.get(&key) {
            return hit.clone();
        }
        if path.contains(&key) || path.len() >= MAX_CHAIN_DEPTH {
            // A cycle (or runaway chain). Not memoized: it depends on the path taken.
            return Canon::Uncertain;
        }
        path.push(key.clone());
        let result = self.canonicalize_step(file, name, path, memo);
        path.pop();
        // Only a path-independent outcome may be reused. `Uncertain` can come from a cycle
        // that exists only relative to this path, so it is recomputed.
        if !matches!(result, Canon::Uncertain) {
            memo.insert(key, result.clone());
        }
        result
    }

    fn canonicalize_step(
        &self,
        file: &str,
        name: &str,
        path: &mut Vec<(String, String)>,
        memo: &mut Memo,
    ) -> Canon {
        let Some(module) = self.modules.get(file) else {
            return Canon::Absent;
        };
        let named = module.named.get(name);
        let is_own = module.own.contains(name);
        // A name that is both declared and re-exported is a syntax error in a real module.
        // Whatever the parse saw, do not guess.
        if is_own && named.is_some() {
            return Canon::Uncertain;
        }
        if is_own {
            return Canon::Found(Found {
                origin_file: file.to_string(),
                origin_name: name.to_string(),
                hops: Vec::new(),
            });
        }
        if let Some(edges) = named {
            let [edge] = edges.as_slice() else {
                return Canon::Uncertain;
            };
            let Some(target) = &edge.target else {
                return Canon::Absent;
            };
            return match self.canonicalize(target, &edge.origin, path, memo) {
                Canon::Found(mut f) => {
                    f.hops.insert(
                        0,
                        Hop {
                            file: file.to_string(),
                            line: edge.line,
                        },
                    );
                    Canon::Found(f)
                }
                other => other,
            };
        }
        // `export *` never forwards `default`.
        if name == "default" || module.stars.is_empty() {
            return Canon::Absent;
        }
        let mut hit: Option<Found> = None;
        let mut opaque = false;
        for (target, line) in &module.stars {
            let Some(target) = target else {
                opaque = true;
                continue;
            };
            match self.canonicalize(target, name, path, memo) {
                Canon::Found(mut f) => {
                    if hit.is_some() {
                        return Canon::Uncertain; // two star sources supply the name
                    }
                    f.hops.insert(
                        0,
                        Hop {
                            file: file.to_string(),
                            line: *line,
                        },
                    );
                    hit = Some(f);
                }
                Canon::Absent => {}
                Canon::Uncertain => return Canon::Uncertain,
            }
        }
        match hit {
            // An external star source might supply the same name: not provable either way.
            Some(_) if opaque => Canon::Uncertain,
            Some(f) => Canon::Found(f),
            None => Canon::Absent,
        }
    }

    /// Every named/default import in the repo, canonicalized, grouped by canonical symbol.
    fn collect_usages(
        &self,
        files: &[(String, String)],
        resolver: &EcmaResolver<'_>,
    ) -> BTreeMap<(String, String), Vec<Usage>> {
        let mut groups: BTreeMap<(String, String), Vec<Usage>> = BTreeMap::new();
        let mut memo = Memo::new();
        for (importer, content) in files {
            let Some(lang) = ecma_lang(importer) else {
                continue;
            };
            for imp in extract::imports(lang, content) {
                let name = match (imp.kind, imp.names.as_slice()) {
                    (ImportKind::Named, [n]) => n.clone(),
                    (ImportKind::Default, _) => "default".to_string(),
                    _ => continue,
                };
                let Some(target) = resolver.resolve(importer, &imp.specifier) else {
                    continue;
                };
                if target == *importer {
                    continue;
                }
                let Canon::Found(found) =
                    self.canonicalize(&target, &name, &mut Vec::new(), &mut memo)
                else {
                    continue;
                };
                groups
                    .entry((found.origin_file, found.origin_name))
                    .or_default()
                    .push(Usage {
                        importer: importer.clone(),
                        target,
                        hops: found.hops,
                    });
            }
        }
        groups
    }
}

fn ecma_lang(path: &str) -> Option<SourceLang> {
    match extract::lang_for_path(path) {
        Some(l @ (SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript)) => Some(l),
        _ => None,
    }
}

/// One import of a canonical symbol: who imported it, which resolved file they reached it
/// through, and the re-export statements between that file and the origin.
struct Usage {
    importer: String,
    target: String,
    hops: Vec<Hop>,
}

// ─── the verdict ────────────────────────────────────────────────────────────────────────

fn judge_group(origin_file: &str, name: &str, group: &[Usage]) -> Option<ArchViolation> {
    // Every module that re-exports this symbol, on any importer's chain. Its directory subtree
    // is "inside the barrel", where importing siblings directly is the convention.
    let barrel_dirs: BTreeSet<&str> = group
        .iter()
        .flat_map(|u| u.hops.iter().map(|h| directory_of(&h.file)))
        .collect();

    let counted: Vec<&Usage> = group
        .iter()
        .filter(|u| !is_test_path(&u.importer))
        .filter(|u| {
            !barrel_dirs
                .iter()
                .any(|dir| is_inside_dir(&u.importer, dir))
        })
        .collect();

    let mut by_target: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for u in &counted {
        by_target
            .entry(u.target.as_str())
            .or_default()
            .insert(u.importer.as_str());
    }
    if by_target.len() < 2 {
        return None;
    }

    // The re-export statements actually carrying the split importers. Attributed to the first
    // by (file, line) so the finding is stable.
    let hops: BTreeSet<&Hop> = counted.iter().flat_map(|u| u.hops.iter()).collect();
    let first = hops.iter().next()?;

    let paths = by_target
        .iter()
        .map(|(target, importers)| describe_path(target, importers, origin_file))
        .collect::<Vec<_>>()
        .join("; ");
    let sites = hops
        .iter()
        .map(|h| format!("{}:{}", h.file, h.line))
        .collect::<Vec<_>>()
        .join(", ");
    let subject = if name == "default" {
        format!("The default export of `{origin_file}`")
    } else {
        format!("`{name}` (defined in `{origin_file}`)")
    };
    let message = format!(
        "{subject} is imported by code outside the re-exporting module's folder through {} \
         different paths: {paths}. The re-export at {sites} is what makes each path work. \
         Import it from one home, the origin or a single barrel, and delete the other path.",
        by_target.len(),
    );
    Some(ArchViolation {
        rule_id: RULE_ONE_IMPORT_PATH.to_string(),
        file: first.file.clone(),
        line: first.line,
        object: Some(name.to_string()),
        message,
        // The corpus rule's `max_severity = "low"` is expected to clamp this at report time;
        // the checker contract forbids emitting `low` directly.
        severity: SEVERITY_MEDIUM,
    })
}

/// `` `path` (the origin, used by a, b, c and 2 more) ``.
fn describe_path(target: &str, importers: &BTreeSet<&str>, origin_file: &str) -> String {
    let shown: Vec<&str> = importers
        .iter()
        .take(IMPORTERS_SHOWN_PER_PATH)
        .copied()
        .collect();
    let more = importers.len().saturating_sub(shown.len());
    let mut users = shown.join(", ");
    if more > 0 {
        users.push_str(&format!(" and {more} more"));
    }
    let role = if target == origin_file {
        "the origin, "
    } else {
        ""
    };
    format!("`{target}` ({role}used by {users})")
}

fn directory_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// Whether `path` lies in `dir`'s subtree. The empty dir is the repo root, i.e. everything.
fn is_inside_dir(path: &str, dir: &str) -> bool {
    dir.is_empty() || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// Test and fixture locations, which reach internals directly by design. The same convention
/// as the gateway's `is_test_or_fixture_path`, restricted to what applies to ECMA files
/// (`camerata-checks` cannot depend on the gateway crate, and no helper exists here).
fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut segs: Vec<&str> = lower.split('/').collect();
    let Some(file) = segs.pop() else {
        return false;
    };
    if segs
        .iter()
        .any(|s| matches!(*s, "tests" | "test" | "__tests__"))
    {
        return true;
    }
    file.rfind('.').is_some_and(|dot| {
        let stem = &file[..dot];
        stem.ends_with(".test") || stem.ends_with(".spec") || stem.ends_with("_test")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
    }

    fn run(f: &[(String, String)]) -> Vec<ArchViolation> {
        OneImportPathChecker.check(&RepoView {
            spec: "test/repo",
            files: f,
        })
    }

    /// A barrel `lib/index.ts` over the origin `lib/x.ts`, with the given re-export line.
    fn lib_with(reexport: &str, extra: Vec<(&str, &str)>) -> Vec<(String, String)> {
        let mut v = vec![
            ("lib/x.ts", "export function a() {}\n"),
            ("lib/index.ts", reexport),
        ];
        v.extend(extra);
        files(v)
    }

    // ── positives ──────────────────────────────────────────────────────────────

    #[test]
    fn symbol_imported_from_origin_and_from_barrel_is_flagged() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/two.ts", "import { a } from '../lib/x';\n"),
            ],
        );
        let vs = run(&f);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        let v = &vs[0];
        assert_eq!(v.rule_id, RULE_ONE_IMPORT_PATH);
        assert_eq!(v.file, "lib/index.ts");
        assert_eq!(v.line, 1);
        assert_eq!(v.severity, SEVERITY_MEDIUM);
        assert_eq!(v.object.as_deref(), Some("a"));
        for needle in [
            "`a`",
            "lib/x.ts",
            "lib/index.ts",
            "app/one.ts",
            "app/two.ts",
        ] {
            assert!(
                v.message.contains(needle),
                "{needle} missing: {}",
                v.message
            );
        }
        assert!(
            v.message.contains("Import it from one home"),
            "{}",
            v.message
        );
    }

    #[test]
    fn aliased_re_export_is_followed_to_the_origin_name() {
        let f = lib_with(
            "// barrel\nexport { a as b } from './x';\n",
            vec![
                ("app/one.ts", "import { b } from '../lib';\n"),
                ("app/two.ts", "import { a } from '../lib/x';\n"),
            ],
        );
        let vs = run(&f);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].file, "lib/index.ts");
        assert_eq!(vs[0].line, 2);
    }

    #[test]
    fn star_re_export_is_followed_to_the_origin() {
        let f = lib_with(
            "export * from './x';\n",
            vec![
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/two.ts", "import { a } from '../lib/x';\n"),
            ],
        );
        let vs = run(&f);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].file, "lib/index.ts");
    }

    #[test]
    fn default_export_re_exported_by_name_is_flagged() {
        let f = files(vec![
            ("lib/x.ts", "export default function widget() {}\n"),
            ("lib/index.ts", "export { default as Widget } from './x';\n"),
            ("app/one.ts", "import { Widget } from '../lib';\n"),
            ("app/two.ts", "import Widget from '../lib/x';\n"),
        ]);
        let vs = run(&f);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].object.as_deref(), Some("default"));
        assert!(
            vs[0].message.contains("default export"),
            "{}",
            vs[0].message
        );
    }

    #[test]
    fn a_barrel_of_barrels_is_followed_and_every_path_is_listed() {
        let f = files(vec![
            ("pkg/lib/x.ts", "export const a = 1;\n"),
            ("pkg/lib/index.ts", "export { a } from './x';\n"),
            ("pkg/index.ts", "export * from './lib';\n"),
            ("app/one.ts", "import { a } from '../pkg';\n"),
            ("app/two.ts", "import { a } from '../pkg/lib';\n"),
            ("app/three.ts", "import { a } from '../pkg/lib/x';\n"),
        ]);
        let vs = run(&f);
        assert_eq!(vs.len(), 1, "{vs:#?}");
        for needle in ["pkg/index.ts", "pkg/lib/index.ts", "pkg/lib/x.ts"] {
            assert!(
                vs[0].message.contains(needle),
                "{needle}: {}",
                vs[0].message
            );
        }
    }

    #[test]
    fn two_different_barrels_over_one_origin_are_flagged() {
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            ("one/index.ts", "export { a } from '../lib/x';\n"),
            ("two/index.ts", "export { a } from '../lib/x';\n"),
            ("app/p.ts", "import { a } from '../one';\n"),
            ("app/q.ts", "import { a } from '../two';\n"),
        ]);
        assert_eq!(run(&f).len(), 1);
    }

    #[test]
    fn js_jsx_and_mjs_files_are_covered() {
        let f = files(vec![
            ("lib/x.js", "export function a() {}\n"),
            ("lib/index.js", "export { a } from './x.js';\n"),
            ("app/one.jsx", "import { a } from '../lib';\n"),
            ("app/two.mjs", "import { a } from '../lib/x';\n"),
        ]);
        assert_eq!(run(&f).len(), 1);
    }

    #[test]
    fn aliased_and_unaliased_names_of_one_origin_group_together() {
        // M exports the symbol as `b`; a second barrel exports it as `a`.
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            ("m/index.ts", "export { a as b } from '../lib/x';\n"),
            ("app/one.ts", "import { b } from '../m';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert_eq!(run(&f).len(), 1);
    }

    #[test]
    fn two_split_symbols_yield_one_violation_each() {
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\nexport const b = 2;\n"),
            ("lib/index.ts", "export { a, b } from './x';\n"),
            (
                "app/one.ts",
                "import { a } from '../lib';\nimport { b } from '../lib';\n",
            ),
            (
                "app/two.ts",
                "import { a } from '../lib/x';\nimport { b } from '../lib/x';\n",
            ),
        ]);
        let vs = run(&f);
        assert_eq!(vs.len(), 2, "{vs:#?}");
        let names: BTreeSet<_> = vs.iter().filter_map(|v| v.object.clone()).collect();
        assert_eq!(names, BTreeSet::from(["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn importer_list_is_capped_and_says_how_many_more() {
        let mut v = vec![
            ("lib/x.ts", "export const a = 1;\n"),
            ("lib/index.ts", "export { a } from './x';\n"),
            ("app/direct.ts", "import { a } from '../lib/x';\n"),
        ];
        let names: Vec<String> = (0..5).map(|i| format!("app/via{i}.ts")).collect();
        for n in &names {
            v.push((n.as_str(), "import { a } from '../lib';\n"));
        }
        let vs = run(&files(v));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert!(vs[0].message.contains("and 2 more"), "{}", vs[0].message);
        assert!(!vs[0].message.contains("app/via4.ts"), "{}", vs[0].message);
    }

    #[test]
    fn message_is_client_voice_without_dashes_or_tool_names() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/two.ts", "import { a } from '../lib/x';\n"),
            ],
        );
        let m = &run(&f)[0].message;
        assert!(!m.contains('\u{2014}') && !m.contains('\u{2013}'), "{m}");
        for banned in ["camerata", "Camerata", "eslint", "ESLint", "Semgrep"] {
            assert!(!m.contains(banned), "{banned} in {m}");
        }
    }

    // ── safe twins ─────────────────────────────────────────────────────────────

    #[test]
    fn safe_twin_every_outside_importer_uses_the_one_barrel() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/two.ts", "import { a } from '../lib/index';\n"),
                ("other/three.ts", "import { a } from '../lib';\n"),
            ],
        );
        assert!(run(&f).is_empty());
    }

    #[test]
    fn safe_twin_barrel_internal_sibling_imports_plus_outside_barrel_users() {
        let f = lib_with(
            "export { a } from './x';\nexport { y } from './y';\n",
            vec![
                (
                    "lib/y.ts",
                    "import { a } from './x';\nexport const y = a;\n",
                ),
                ("lib/deep/z.ts", "import { a } from '../x';\n"),
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/two.ts", "import { a } from '../lib';\n"),
            ],
        );
        assert!(run(&f).is_empty());
    }

    #[test]
    fn safe_twin_relative_position_spelling_of_the_same_file() {
        // `./utils` from a/ and `../a/utils` from b/ are one file. A barrel exists but nobody
        // imports through it, so there is exactly one resolved target.
        let f = files(vec![
            ("a/utils.ts", "export const u = 1;\n"),
            ("a/consumer.ts", "import { u } from './utils';\n"),
            ("b/consumer.ts", "import { u } from '../a/utils';\n"),
            ("shared/index.ts", "export { u } from '../a/utils';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn safe_twin_tsconfig_alias_versus_relative_path_to_the_same_file() {
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
            ),
            ("src/lib/x.ts", "export const a = 1;\n"),
            ("src/lib/index.ts", "export { a } from './x';\n"),
            ("src/app/one.ts", "import { a } from '@/lib/x';\n"),
            ("src/app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn tsconfig_alias_to_a_barrel_still_counts_as_the_barrel_path() {
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
            ),
            ("src/lib/x.ts", "export const a = 1;\n"),
            ("src/lib/index.ts", "export { a } from './x';\n"),
            ("src/app/one.ts", "import { a } from '@/lib';\n"),
            ("src/app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert_eq!(run(&f).len(), 1);
    }

    #[test]
    fn safe_twin_same_name_different_origin_files() {
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            ("lib/index.ts", "export { a } from './x';\n"),
            ("other/z.ts", "export const a = 2;\n"),
            ("app/one.ts", "import { a } from '../lib';\n"),
            ("app/two.ts", "import { a } from '../other/z';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn safe_twin_test_files_importing_deep_are_excluded() {
        for test_path in [
            "app/one.test.ts",
            "app/one.spec.tsx",
            "app/__tests__/one.ts",
            "tests/one.ts",
            "test/one.ts",
            "app/tests/helpers/one.ts",
        ] {
            let f = lib_with(
                "export { a } from './x';\n",
                vec![
                    ("app/real.ts", "import { a } from '../lib';\n"),
                    (test_path, "import { a } from '../lib/x';\n"),
                ],
            );
            assert!(run(&f).is_empty(), "{test_path} must be excluded");
        }
    }

    #[test]
    fn a_non_test_path_with_test_in_a_longer_name_is_still_counted() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![
                ("app/real.ts", "import { a } from '../lib';\n"),
                ("app/contest/two.ts", "import { a } from '../../lib/x';\n"),
            ],
        );
        assert_eq!(run(&f).len(), 1);
    }

    #[test]
    fn aliased_re_export_trap_own_a_and_aliased_b_are_different_symbols() {
        // M has its OWN `a` and also `export { a as b } from './x'`. Importers of `a` from M
        // and of `a` from x are different symbols and must not be grouped.
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            (
                "lib/m.ts",
                "export const a = 99;\nexport { a as b } from './x';\n",
            ),
            ("app/one.ts", "import { a } from '../lib/m';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert!(run(&f).is_empty());
        // ...while `b` from M really is x's `a`, so b-from-M plus a-from-x IS a split.
        let g = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            (
                "m/m.ts",
                "export const a = 99;\nexport { a as b } from '../lib/x';\n",
            ),
            ("app/one.ts", "import { b } from '../m/m';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert_eq!(run(&g).len(), 1);
    }

    #[test]
    fn own_export_wins_over_a_star_re_export_of_the_same_name() {
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            (
                "m/index.ts",
                "export const a = 2;\nexport * from '../lib/x';\n",
            ),
            ("app/one.ts", "import { a } from '../m';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn ambiguous_star_sources_are_skipped() {
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            ("lib/y.ts", "export const a = 2;\n"),
            (
                "lib/index.ts",
                "export * from './x';\nexport * from './y';\n",
            ),
            ("app/one.ts", "import { a } from '../lib';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn an_unresolvable_star_source_makes_star_derived_names_uncertain() {
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            (
                "lib/index.ts",
                "export * from './x';\nexport * from 'some-external-package';\n",
            ),
            ("app/one.ts", "import { a } from '../lib';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn star_re_exports_never_carry_default() {
        let f = files(vec![
            ("lib/x.ts", "export default function d() {}\n"),
            ("lib/index.ts", "export * from './x';\n"),
            ("app/one.ts", "import D from '../lib';\n"),
            ("app/two.ts", "import D from '../lib/x';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn star_reaching_one_origin_through_two_levels_is_not_ambiguous() {
        // Only ONE star source supplies `a` (the other supplies `b`).
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\n"),
            ("lib/y.ts", "export const b = 1;\n"),
            (
                "lib/index.ts",
                "export * from './x';\nexport * from './y';\n",
            ),
            ("app/one.ts", "import { a } from '../lib';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ]);
        assert_eq!(run(&f).len(), 1);
    }

    #[test]
    fn star_re_export_cycle_terminates_and_is_skipped() {
        let f = files(vec![
            ("lib/m.ts", "export * from './n';\n"),
            ("lib/n.ts", "export * from './m';\n"),
            ("app/one.ts", "import { a } from '../lib/m';\n"),
            ("app/two.ts", "import { a } from '../lib/n';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn named_re_export_cycle_terminates_and_is_skipped() {
        let f = files(vec![
            ("lib/m.ts", "export { a } from './n';\n"),
            ("lib/n.ts", "export { a } from './m';\n"),
            ("app/one.ts", "import { a } from '../lib/m';\n"),
            ("app/two.ts", "import { a } from '../lib/n';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn namespace_side_effect_dynamic_and_re_export_statements_are_not_importers() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/ns.ts", "import * as x from '../lib/x';\n"),
                ("app/fx.ts", "import '../lib/x';\n"),
                ("app/dyn.ts", "const m = import('../lib/x');\n"),
                ("app/cjs.js", "const m = require('../lib/x');\n"),
                // A re-export statement is a graph edge, not a consumer.
                ("other/barrel.ts", "export { a } from '../lib/x';\n"),
            ],
        );
        assert!(run(&f).is_empty());
    }

    #[test]
    fn unresolvable_and_external_specifiers_are_ignored() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/ext.ts", "import { a } from 'some-package';\n"),
                ("app/missing.ts", "import { a } from '../nowhere';\n"),
            ],
        );
        assert!(run(&f).is_empty());
    }

    #[test]
    fn a_symbol_that_is_not_defined_in_the_target_is_ignored() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![
                ("app/one.ts", "import { a } from '../lib';\n"),
                ("app/two.ts", "import { nothere } from '../lib/x';\n"),
            ],
        );
        assert!(run(&f).is_empty());
    }

    #[test]
    fn a_repo_root_re_exporter_excludes_every_importer() {
        // A re-exporter at the repo root (directory "") owns the whole repo as its subtree.
        // Documented false negative.
        let f = files(vec![
            ("src/x.ts", "export const a = 1;\n"),
            ("index.ts", "export { a } from './src/x';\n"),
            ("app/one.ts", "import { a } from '../index';\n"),
            ("app/two.ts", "import { a } from '../src/x';\n"),
        ]);
        assert!(run(&f).is_empty());
    }

    #[test]
    fn importing_the_same_name_twice_from_one_path_is_one_path() {
        let f = lib_with(
            "export { a } from './x';\n",
            vec![(
                "app/one.ts",
                "import { a } from '../lib';\nimport { a as again } from '../lib';\n",
            )],
        );
        assert!(run(&f).is_empty());
    }

    // ── robustness ─────────────────────────────────────────────────────────────

    #[test]
    fn malformed_input_does_not_panic() {
        let f = files(vec![
            ("lib/broken.ts", "export { a as } from"),
            ("lib/broken2.ts", "export * from"),
            ("lib/index.ts", "export {{{ not typescript"),
            ("app/one.ts", "import { from '../lib"),
            ("app/two.tsx", "import {a} from '../lib/broken'; <<<>>>"),
            ("app/bin.js", "\u{0}\u{1}\u{2}"),
            ("tsconfig.json", "{ not json"),
        ]);
        let _ = run(&f);
    }

    #[test]
    fn empty_file_set_does_not_panic() {
        assert!(run(&[]).is_empty());
    }

    #[test]
    fn output_is_deterministic() {
        let f = files(vec![
            ("lib/x.ts", "export const a = 1;\nexport const b = 2;\n"),
            ("lib/index.ts", "export { a, b } from './x';\n"),
            ("app/one.ts", "import { a, b } from '../lib';\n"),
            ("app/two.ts", "import { a, b } from '../lib/x';\n"),
        ]);
        assert_eq!(run(&f), run(&f));
    }

    // ── contract ───────────────────────────────────────────────────────────────

    #[test]
    fn checker_answers_its_rule_id_and_scopes_to_ecma_files() {
        assert_eq!(OneImportPathChecker.rule_ids(), &[RULE_ONE_IMPORT_PATH]);
        assert!(crate::arch_checker::checker_applies(
            &OneImportPathChecker,
            &files(vec![("src/a.ts", "")])
        ));
        for ext in ["tsx", "js", "jsx", "mjs", "cjs"] {
            let p = format!("src/a.{ext}");
            assert!(
                crate::arch_checker::checker_applies(&OneImportPathChecker, &files(vec![(&p, "")])),
                "{ext}"
            );
        }
        assert!(!crate::arch_checker::checker_applies(
            &OneImportPathChecker,
            &files(vec![("README.md", ""), ("src/a.py", ""), ("src/a.rs", "")])
        ));
    }
}
