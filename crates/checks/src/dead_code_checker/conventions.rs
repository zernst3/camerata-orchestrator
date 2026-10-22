//! Files loaded by convention rather than by import: the file-system routers of web
//! frameworks (`G4`), folders that people and tools run by hand (`G5`), and Python's
//! autodiscovered modules (`G6`). Detection is by dependency name in the nearest
//! `package.json`, or by the framework's config file beside it, so a folder called `pages/`
//! in a repository with no router is still checked.
//!
//! A framework this list does not know is covered by nothing but the mention guard: a
//! convention-loaded file in an unknown framework is still unmentioned and WOULD be reported.
//! Extend the list as frameworks are met; a `keep-file:` marker bounds the residual risk.

use std::collections::HashMap;

use super::manifests::{ManifestDirs, Package, Packages};
use super::Family;

/// Which routers and platforms a package uses.
#[derive(Debug, Default)]
struct Frameworks {
    next: bool,
    remix: bool,
    sveltekit: bool,
    nuxt: bool,
    /// Nuxt's app directory (`""` or `app`), relative to the package.
    nuxt_app_dir: &'static str,
    astro: bool,
    expo_router: bool,
    gatsby: bool,
    docusaurus: bool,
    ember: bool,
    tanstack_router: bool,
    vercel: bool,
    /// `wrangler.toml` or `firebase.json`: `functions/` is loaded by the platform.
    cloud_functions: bool,
}

/// Next.js `app/` files that carry meaning; any other file there is ordinary code.
const NEXT_APP_CONVENTIONS: &[&str] = &[
    "page",
    "layout",
    "loading",
    "error",
    "not-found",
    "route",
    "template",
    "default",
    "global-error",
    "sitemap",
    "robots",
    "opengraph-image",
    "twitter-image",
    "icon",
    "apple-icon",
    "manifest",
];

/// Nuxt directories whose files are auto-imported or auto-registered.
const NUXT_DIRS: &[&str] = &[
    "pages/",
    "layouts/",
    "middleware/",
    "plugins/",
    "composables/",
    "utils/",
    "components/",
    "stores/",
    "server/api/",
    "server/routes/",
    "server/middleware/",
    "server/plugins/",
    "server/utils/",
];

/// `G5`: folders whose files are run by hand, read by people or loaded by a tool by glob.
const TOOL_DIRS: &[&str] = &[
    "config",
    "scripts",
    "tools",
    "bin",
    "docs",
    "examples",
    "example",
    "samples",
    "benchmarks",
    "migrations",
    "seeds",
    "db",
    "prisma",
    "supabase",
    ".storybook",
    "cypress",
    "playwright",
    "e2e",
    "public",
    "static",
    "assets",
];

/// Folders whose files a CLI or migrator loads by glob, in any language and at ANY depth
/// (`src/migrations/`, `database/seeders/`, `contracts/deploy/`): TypeORM, Knex, Adonis,
/// Sequelize, hardhat-deploy and their kin. The corpus rule promises migrations are never
/// reported, so this is not limited to the first folder under the package like [`TOOL_DIRS`].
const ANY_DEPTH_TOOL_DIRS: &[&str] = &["migrations", "seeders", "seeds", "deploy"];

/// `G6`: Python modules loaded by name by their framework or tool.
const PYTHON_BASENAMES: &[&str] = &[
    "__init__.py",
    "__main__.py",
    "conftest.py",
    "setup.py",
    "manage.py",
    "wsgi.py",
    "asgi.py",
    "settings.py",
    "urls.py",
    "celery.py",
    "tasks.py",
    "admin.py",
    "apps.py",
    "models.py",
    "forms.py",
    "serializers.py",
    "views.py",
    "tests.py",
    "checks.py",
    "locustfile.py",
    "noxfile.py",
    "fabfile.py",
    "gunicorn.conf.py",
    "conf.py",
];

const PYTHON_DIRS: &[&str] = &[
    "migrations",
    "management",
    "templatetags",
    "dags",
    "spiders",
    "commands",
    "alembic",
    "versions",
    "settings",
    "fixtures",
];

pub(super) struct Conventions<'a> {
    packages: &'a Packages,
    frameworks: HashMap<String, Frameworks>,
    manifest_dirs: ManifestDirs,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// `path` relative to `dir`, when it lies inside it.
fn relative_to<'p>(path: &'p str, dir: &str) -> Option<&'p str> {
    if dir.is_empty() {
        Some(path)
    } else {
        path.strip_prefix(dir)?.strip_prefix('/')
    }
}

impl<'a> Conventions<'a> {
    pub(super) fn new(files: &[(String, String)], packages: &'a Packages) -> Self {
        let mut by_dir: HashMap<&str, Vec<&str>> = HashMap::new();
        for (path, _) in files {
            by_dir.entry(dir_of(path)).or_default().push(basename(path));
        }
        let has = |dir: &str, test: &dyn Fn(&str) -> bool| {
            by_dir
                .get(dir)
                .is_some_and(|names| names.iter().any(|n| test(n)))
        };
        let mut dirs: Vec<String> = vec![String::new()];
        dirs.extend(packages.dirs());
        dirs.sort();
        dirs.dedup();
        let mut frameworks = HashMap::new();
        for dir in dirs {
            let package = packages.at(&dir);
            let dep = |test: &dyn Fn(&str) -> bool| package.is_some_and(|p| p.depends_on(test));
            let config = |stem: &str| has(&dir, &|n: &str| n.starts_with(&format!("{stem}.")));
            let app_dir = |sub: &str| {
                if dir.is_empty() {
                    sub.to_string()
                } else {
                    format!("{dir}/{sub}")
                }
            };
            let root_file = by_dir
                .get(app_dir("app").as_str())
                .is_some_and(|names| names.iter().any(|n| n.starts_with("root.")));
            let nuxt_in_app = by_dir
                .get(app_dir("app").as_str())
                .is_some_and(|names| names.contains(&"app.vue"));
            let fw = Frameworks {
                next: dep(&|k| k == "next") || config("next.config"),
                remix: dep(&|k| k.starts_with("@remix-run/"))
                    || (dep(&|k| k == "react-router" || k.starts_with("@react-router/"))
                        && root_file),
                sveltekit: dep(&|k| k == "@sveltejs/kit"),
                nuxt: dep(&|k| k == "nuxt") || config("nuxt.config"),
                nuxt_app_dir: if nuxt_in_app { "app" } else { "" },
                astro: dep(&|k| k == "astro"),
                expo_router: dep(&|k| k == "expo-router"),
                gatsby: dep(&|k| k == "gatsby"),
                docusaurus: dep(&|k| k == "@docusaurus/core"),
                ember: dep(&|k| k == "ember-source" || k == "ember-cli"),
                tanstack_router: dep(&|k| {
                    k == "@tanstack/react-router"
                        || k == "@tanstack/router-plugin"
                        || k == "@tanstack/router-cli"
                        || k == "@tanstack/start"
                }),
                vercel: has(&dir, &|n| n == "vercel.json"),
                cloud_functions: has(&dir, &|n| n == "wrangler.toml" || n == "firebase.json"),
            };
            frameworks.insert(dir, fw);
        }
        Self {
            packages,
            frameworks,
            manifest_dirs: ManifestDirs::load(files),
        }
    }

    /// Whether a framework, platform or language convention loads `path` by itself.
    pub(super) fn is_convention_path(&self, path: &str, family: Family) -> bool {
        match family {
            Family::Ecma if self.framework_loads(path) => return true,
            Family::Python if python_loads(path) => return true,
            _ => {}
        }
        in_any_depth_tool_dir(path) || self.in_tool_directory(path)
    }

    fn framework_loads(&self, path: &str) -> bool {
        let package_dir = self.packages.nearest(path).map_or("", |p| p.dir.as_str());
        let Some(fw) = self.frameworks.get(package_dir) else {
            return false;
        };
        let Some(rel) = relative_to(path, package_dir) else {
            return false;
        };
        let file = basename(rel);
        let stem = file.split('.').next().unwrap_or(file);
        let under = |prefix: &str| rel.starts_with(prefix);
        let in_src_dir_named = |name: &str| {
            rel.strip_prefix("src/")
                .is_some_and(|r| !r.contains('/') && (r.starts_with(&format!("{name}."))))
        };
        if fw.next {
            if under("pages/") || under("src/pages/") {
                return true;
            }
            if (under("app/") || under("src/app/")) && NEXT_APP_CONVENTIONS.contains(&stem) {
                return true;
            }
        }
        if fw.remix {
            let direct_in_app = |name: &str| {
                rel.strip_prefix("app/")
                    .is_some_and(|r| !r.contains('/') && r.starts_with(&format!("{name}.")))
            };
            if under("app/routes/")
                || direct_in_app("root")
                || direct_in_app("entry")
                || direct_in_app("routes")
            {
                return true;
            }
        }
        if fw.sveltekit
            && (under("src/routes/")
                || under("src/params/")
                || under("src/hooks/")
                || in_src_dir_named("hooks")
                || in_src_dir_named("service-worker"))
        {
            return true;
        }
        if fw.nuxt {
            let app = fw.nuxt_app_dir;
            if let Some(in_app) = relative_to(rel, app) {
                if NUXT_DIRS.iter().any(|d| in_app.starts_with(d)) {
                    return true;
                }
                if !in_app.contains('/')
                    && (in_app.starts_with("app.") || in_app.starts_with("error."))
                {
                    return true;
                }
            }
        }
        if fw.astro && (under("src/pages/") || under("src/content/config.")) {
            return true;
        }
        if fw.expo_router && under("app/") {
            return true;
        }
        if fw.gatsby && (under("src/pages/") || under("src/templates/")) {
            return true;
        }
        if fw.docusaurus && (under("src/pages/") || under("src/theme/")) {
            return true;
        }
        // Ember's resolver loads routes, controllers, services and components under `app/` by
        // name from the template or the router, never by import.
        // TanStack Router's file-based routes: every file under the routes folder.
        if fw.tanstack_router && (under("src/routes/") || under("app/routes/")) {
            return true;
        }
        if fw.ember && under("app/") {
            return true;
        }
        if fw.vercel && under("api/") {
            return true;
        }
        if under("netlify/functions/") || under("netlify/edge-functions/") {
            return true;
        }
        fw.cloud_functions && under("functions/")
    }

    /// `G5`: the first folder under the nearest manifest (or under the repository root) names a
    /// directory people or tools use directly.
    fn in_tool_directory(&self, path: &str) -> bool {
        let nearest = self.manifest_dirs.nearest(path);
        [nearest, ""].iter().any(|base| {
            relative_to(path, base)
                .and_then(|rel| rel.split_once('/'))
                .is_some_and(|(first, _)| TOOL_DIRS.contains(&first))
        })
    }
}

fn in_any_depth_tool_dir(path: &str) -> bool {
    path.split('/')
        .rev()
        .skip(1)
        .any(|seg| ANY_DEPTH_TOOL_DIRS.contains(&seg))
}

fn python_loads(path: &str) -> bool {
    if PYTHON_BASENAMES.contains(&basename(path)) {
        return true;
    }
    let mut segments: Vec<&str> = path.split('/').collect();
    segments.pop();
    segments.iter().any(|s| PYTHON_DIRS.contains(s))
}

impl Packages {
    /// Directories holding a parsed `package.json`.
    pub(super) fn dirs(&self) -> Vec<String> {
        self.iter().map(|p| p.dir.clone()).collect()
    }

    /// The package whose manifest sits exactly in `dir`.
    pub(super) fn at(&self, dir: &str) -> Option<&Package> {
        self.iter().find(|p| p.dir == dir)
    }
}
