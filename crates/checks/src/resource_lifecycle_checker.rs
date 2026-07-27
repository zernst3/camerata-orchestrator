//! `ResourceLifecycleChecker`: Pass 4c — the SPAWN facet of `ARCH-RESOURCE-LIFECYCLE-1`. See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group D and
//! `crates/rules/principles/universal/arch-resource-lifecycle-1.toml`.
//!
//! # What this checks (and what it deliberately does NOT)
//!
//! The rule's own `qualifies` text splits it in two: "every child-process spawn carries a
//! kill-on-drop / kill-on-cancel disposition ... that part is greppable per spawn site" versus
//! "children that outlive a single await ... are tracked and killed on shutdown, and every
//! temporary file ... uses RAII auto-cleanup ... verified by review." This checker answers
//! ONLY the first (greppable) half, for Rust/`tokio` (v1 scope; Node is a future language, per
//! the design table). It is a custom `syn` AST scan, not built on the generic
//! [`crate::extract`] utilities — chain/variable correlation is bespoke to this one rule, so it
//! lives entirely in this checker, per the design's "no shared enriched model" stance.
//!
//! # Model: two call shapes, one verdict
//!
//! 1. **Inline chain**: `Command::new("x").arg("y").spawn()` — receiver of a terminal call
//!    (`.spawn()`/`.output()`/`.status()`) recursively unwraps, through the SAME expression's
//!    method-call chain, to a `Command::new(...)` call. `.kill_on_drop(true)` anywhere in that
//!    same chain exempts it.
//! 2. **Variable-tracked** (the shape this codebase's own `subprocess.rs` actually uses):
//!    `let mut cmd = Command::new("x"); cmd.arg("y"); cmd.kill_on_drop(true); cmd.spawn();` —
//!    a `let`-bound variable whose initializer roots at `Command::new(...)` is tracked
//!    FILE-WIDE (not per-function-scope — a deliberate simplification, see below); any
//!    `<var>.kill_on_drop(true)` call anywhere in the file (at bind time or as a separate
//!    statement) marks it protected; any `<var>.{spawn,output,status}()` call is a candidate
//!    violation unless the variable is protected.
//!
//! `std::process::Command` (no `kill_on_drop` method at all — this is a `tokio::process`-only
//! API) is recognized by an explicit `std::` path segment and SKIPPED (false-negative safe):
//! flagging code for not calling a method that doesn't exist on its own receiver type would be
//! a fabricated finding, not a real one.
//!
//! # File-wide (not function-scoped) variable tracking — a documented simplification
//!
//! A real per-function-scope analysis would need to track local `fn`/closure boundaries
//! per-variable. This pass tracks `Command::new`-bound variable NAMES across the WHOLE file
//! instead: if TWO different functions in the same file both use a variable named `cmd`, and
//! ONE of them calls `.kill_on_drop(true)`, the other is (incorrectly) also treated as
//! protected. This is a deliberate trade against the "fail false-negative, never fabricate a
//! finding" mandate (`docs/design/2026-07-27_ast-extractor-layer.md`'s D3 discipline, extended
//! here): the coarsening can only ever SUPPRESS a real violation, never invent one, so it is
//! the safe direction to err in. Revisit with real per-function scoping if this proves too
//! coarse in practice.
//!
//! # D3: `needs-review` ALWAYS (not a config gate)
//!
//! This facet is NOT config-gated — the rule's own TOML draws the "greppable vs. review"
//! line by design, not by missing config. It therefore opts into
//! [`ArchChecker::advisory_coexisting`] unconditionally (findings are real, structural,
//! `syn`-verified — but the corpus rule's own scope for this facet was never meant to be the
//! FULL verdict on `ARCH-RESOURCE-LIFECYCLE-1`, only its mechanically-checkable slice), exactly
//! mirroring how the deleted interim `HandlerNoDbChecker` used the same mechanism for a
//! different reason (there: missing config; here: the rule's own scope split).

use std::collections::HashSet;

use syn::visit::Visit;
use syn::Expr;

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_MEDIUM};

pub const RULE_RESOURCE_LIFECYCLE: &str = "ARCH-RESOURCE-LIFECYCLE-1";

const RULE_IDS: &[&str] = &[RULE_RESOURCE_LIFECYCLE];

const INTEREST_GLOBS: &[&str] = &["**/*.rs"];

/// The `tokio::process::Command` methods that actually launch a child process — `kill_on_drop`
/// must be set on the SAME chain/variable before one of these, not after.
const TERMINAL_METHODS: &[&str] = &["spawn", "output", "status"];

pub struct ResourceLifecycleChecker;

impl ArchChecker for ResourceLifecycleChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        repo.files
            .iter()
            .filter(|(path, _)| crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path))
            .flat_map(|(path, content)| spawn_chain_violations(path, content))
            .collect()
    }

    /// Always advisory — see the module doc's "D3: `needs-review` ALWAYS" section. This is a
    /// rule-design fact, not a per-repo config gap, so (unlike every config-gated checker in
    /// this pass) `config_unsatisfied_for` stays the trait default (`false`) and this instead
    /// overrides `advisory_coexisting`.
    fn advisory_coexisting(&self) -> bool {
        true
    }
}

fn spawn_chain_violations(path: &str, source: &str) -> Vec<ArchViolation> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };

    // Pass 1: collect every `let <ident> = <expr>;` whose initializer roots at
    // `Command::new(...)`, and whether `.kill_on_drop(true)` is already chained into that same
    // initializer expression.
    let mut tracked: HashSet<String> = HashSet::new();
    let mut protected: HashSet<String> = HashSet::new();
    {
        let mut collector = LocalBindingVisitor { tracked: &mut tracked, protected: &mut protected };
        collector.visit_file(&file);
    }

    // Pass 2: a `<var>.kill_on_drop(true)` call ANYWHERE in the file (a separate statement,
    // not just chained at bind time) also marks that tracked variable protected.
    {
        let mut protector = ProtectVisitor { tracked: &tracked, protected: &mut protected };
        protector.visit_file(&file);
    }

    // Pass 3: find every terminal spawn call (inline-chain or tracked-variable) and decide.
    let mut finder = TerminalCallVisitor { path, tracked: &tracked, protected: &protected, out: Vec::new() };
    finder.visit_file(&file);
    finder.out
}

// ─── AST matching helpers ────────────────────────────────────────────────────────────────────

/// Recursively unwrap `expr` to decide whether it roots at a `Command::new(...)` call —
/// following a builder chain's `.receiver` spine through `Paren`/`Try`/`MethodCall` wrappers.
/// A path containing an explicit `std` segment is treated as `std::process::Command` (which has
/// no `kill_on_drop` method at all) and is deliberately EXCLUDED — see the module doc.
fn root_is_command_new(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(p) => root_is_command_new(&p.expr),
        Expr::Try(t) => root_is_command_new(&t.expr),
        Expr::MethodCall(m) => root_is_command_new(&m.receiver),
        Expr::Call(c) => match c.func.as_ref() {
            Expr::Path(p) => path_is_command_new(p),
            _ => false,
        },
        _ => false,
    }
}

fn path_is_command_new(p: &syn::ExprPath) -> bool {
    if p.qself.is_some() {
        return false;
    }
    let segs: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
    let Some(last) = segs.last() else {
        return false;
    };
    if last != "new" {
        return false;
    }
    if segs.iter().any(|s| s == "std") {
        return false; // std::process::Command has no kill_on_drop — not this rule's concern
    }
    segs.iter().any(|s| s == "Command")
}

/// Recursively scan `expr`'s chain spine for a `.kill_on_drop(true)` call.
fn chain_contains_kill_on_drop_true(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(p) => chain_contains_kill_on_drop_true(&p.expr),
        Expr::Try(t) => chain_contains_kill_on_drop_true(&t.expr),
        Expr::MethodCall(m) => {
            if m.method == "kill_on_drop" && is_true_literal(m.args.first()) {
                return true;
            }
            chain_contains_kill_on_drop_true(&m.receiver)
        }
        _ => false,
    }
}

fn is_true_literal(arg: Option<&Expr>) -> bool {
    matches!(
        arg,
        Some(Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. })) if b.value
    )
}

/// A bare single-segment identifier receiver (`db`, `cmd`) — the v1 scope for variable
/// tracking. A `self.field`/richer receiver is a rarer shape for a locally-`let`-bound
/// `Command`; not tracked (a false negative, never a false positive).
fn bare_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            Some(p.path.segments[0].ident.to_string())
        }
        Expr::Paren(p) => bare_ident(&p.expr),
        _ => None,
    }
}

fn pat_ident(pat: &syn::Pat) -> Option<String> {
    match pat {
        syn::Pat::Ident(i) => Some(i.ident.to_string()),
        syn::Pat::Type(t) => pat_ident(&t.pat),
        _ => None,
    }
}

// ─── pass 1: let-binding collection ──────────────────────────────────────────────────────────

struct LocalBindingVisitor<'a> {
    tracked: &'a mut HashSet<String>,
    protected: &'a mut HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for LocalBindingVisitor<'a> {
    fn visit_local(&mut self, node: &'ast syn::Local) {
        if let Some(init) = &node.init {
            if root_is_command_new(&init.expr) {
                if let Some(name) = pat_ident(&node.pat) {
                    if chain_contains_kill_on_drop_true(&init.expr) {
                        self.protected.insert(name.clone());
                    }
                    self.tracked.insert(name);
                }
            }
        }
        syn::visit::visit_local(self, node);
    }
}

// ─── pass 2: standalone kill_on_drop(true) statements ────────────────────────────────────────

struct ProtectVisitor<'a> {
    tracked: &'a HashSet<String>,
    protected: &'a mut HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for ProtectVisitor<'a> {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if node.method == "kill_on_drop" && is_true_literal(node.args.first()) {
            if let Some(name) = bare_ident(&node.receiver) {
                if self.tracked.contains(&name) {
                    self.protected.insert(name);
                }
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

// ─── pass 3: terminal spawn-call verdicts ────────────────────────────────────────────────────

struct TerminalCallVisitor<'a> {
    path: &'a str,
    tracked: &'a HashSet<String>,
    protected: &'a HashSet<String>,
    out: Vec<ArchViolation>,
}

impl<'a, 'ast> Visit<'ast> for TerminalCallVisitor<'a> {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        if TERMINAL_METHODS.contains(&method.as_str()) {
            if root_is_command_new(&node.receiver) {
                // Case A: inline chain, no variable — check kill_on_drop within THIS chain.
                if !chain_contains_kill_on_drop_true(&node.receiver) {
                    self.push(node, &method);
                }
            } else if let Some(name) = bare_ident(&node.receiver) {
                // Case B: a tracked variable — check the file-wide protected set.
                if self.tracked.contains(&name) && !self.protected.contains(&name) {
                    self.push(node, &method);
                }
                // Not a tracked variable at all -> unknown receiver type, skip (false-negative
                // safe: we don't know this is even a `Command`).
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

impl<'a> TerminalCallVisitor<'a> {
    fn push(&mut self, node: &syn::ExprMethodCall, method: &str) {
        self.out.push(ArchViolation {
            rule_id: RULE_RESOURCE_LIFECYCLE.to_string(),
            file: self.path.to_string(),
            line: node.method.span().start().line,
            object: Some(format!("Command::{method}()")),
            severity: SEVERITY_MEDIUM,
            message: format!(
                "a `Command` is spawned via `.{method}()` with no `.kill_on_drop(true)` anywhere in its \
                 builder chain — a dropped, timed-out, or cancelled future will orphan this child process \
                 instead of reaping it (ARCH-RESOURCE-LIFECYCLE-1) [needs review: this facet only checks \
                 the mechanically-greppable spawn-disposition marker; tracked-shutdown for children that \
                 outlive a single await and temp-file RAII cleanup are verified by review, per the rule's \
                 own qualifies text]"
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(files: &'a [(String, String)]) -> RepoView<'a> {
        RepoView { spec: "test/repo", files }
    }

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs.into_iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    // ── fires on a real violation ──────────────────────────────────────────────

    #[test]
    fn inline_chain_without_kill_on_drop_is_flagged() {
        let f = files(vec![(
            "src/x.rs",
            "async fn run() {\n    let _ = Command::new(\"ls\").arg(\"-la\").spawn();\n}\n",
        )]);
        let vs = ResourceLifecycleChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_RESOURCE_LIFECYCLE);
        assert_eq!(vs[0].file, "src/x.rs");
        assert_eq!(vs[0].line, 2);
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
        assert!(vs[0].message.contains("[needs review"), "{}", vs[0].message);
    }

    #[test]
    fn variable_tracked_without_kill_on_drop_is_flagged_matching_real_codebase_style() {
        // Mirrors this repo's OWN `subprocess.rs` shape (before the fix): separate statements,
        // not one long chain.
        let f = files(vec![(
            "src/x.rs",
            "async fn run(program: &str) {\n    let mut cmd = Command::new(program);\n    cmd.arg(\"x\");\n    let _ = cmd.spawn();\n}\n",
        )]);
        let vs = ResourceLifecycleChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].line, 4);
    }

    // ── clean when protected ───────────────────────────────────────────────────

    #[test]
    fn inline_chain_with_kill_on_drop_is_clean() {
        let f = files(vec![(
            "src/x.rs",
            "async fn run() {\n    let _ = Command::new(\"ls\").kill_on_drop(true).spawn();\n}\n",
        )]);
        assert!(ResourceLifecycleChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn variable_tracked_with_kill_on_drop_as_a_separate_statement_is_clean() {
        // Exactly this repo's OWN `subprocess.rs::run_with_heartbeat` shape.
        let f = files(vec![(
            "src/x.rs",
            "async fn run(program: &str) {\n    let mut cmd = Command::new(program);\n    cmd.kill_on_drop(true);\n    let _ = cmd.spawn();\n}\n",
        )]);
        assert!(ResourceLifecycleChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn variable_tracked_with_kill_on_drop_chained_at_bind_time_is_clean() {
        let f = files(vec![(
            "src/x.rs",
            "async fn run(program: &str) {\n    let mut cmd = Command::new(program).kill_on_drop(true);\n    let _ = cmd.spawn();\n}\n",
        )]);
        assert!(ResourceLifecycleChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn output_and_status_terminal_methods_are_also_checked() {
        let f = files(vec![
            ("src/a.rs", "async fn f() {\n    let _ = Command::new(\"ls\").output();\n}\n"),
            ("src/b.rs", "async fn f() {\n    let _ = Command::new(\"ls\").status();\n}\n"),
        ]);
        let vs = ResourceLifecycleChecker.check(&view(&f));
        assert_eq!(vs.len(), 2, "{vs:#?}");
    }

    // ── false-negative safeguards ──────────────────────────────────────────────

    #[test]
    fn std_process_command_is_skipped_never_flagged() {
        // std::process::Command has no kill_on_drop method at all — flagging it would be a
        // fabricated finding, not a real one.
        let f = files(vec![(
            "src/x.rs",
            "fn run() {\n    let _ = std::process::Command::new(\"ls\").spawn();\n}\n",
        )]);
        assert!(ResourceLifecycleChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn unrelated_type_with_a_spawn_method_is_never_flagged() {
        // `worker.spawn()` on some unrelated struct — never proven to be a `Command`, so the
        // false-negative-safe outcome is to skip, not guess.
        let f = files(vec![(
            "src/x.rs",
            "fn run(worker: &Worker) {\n    let _ = worker.spawn();\n}\n",
        )]);
        assert!(ResourceLifecycleChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn tokio_spawn_free_function_is_not_a_method_call_and_is_ignored() {
        let f = files(vec![("src/x.rs", "fn run() {\n    tokio::spawn(async {});\n}\n")]);
        assert!(ResourceLifecycleChecker.check(&view(&f)).is_empty());
    }

    // ── adversarial: malformed / truncated / non-UTF8-ish source never panics ─

    #[test]
    fn malformed_source_does_not_panic() {
        let f = files(vec![("src/x.rs", "fn broken( {{{ ??? Command::new(")]);
        let _ = ResourceLifecycleChecker.check(&view(&f));
    }

    #[test]
    fn empty_and_binary_like_source_does_not_panic() {
        let weird = "\u{0}\u{1}\u{FFFD} not rust at all \u{FFFD}";
        let f = files(vec![("src/a.rs", ""), ("src/b.rs", weird)]);
        let _ = ResourceLifecycleChecker.check(&view(&f));
    }

    #[test]
    fn non_rust_file_is_not_scoped_in() {
        let f = files(vec![("src/x.ts", "Command.new('ls').spawn();\n")]);
        assert!(!crate::arch_checker::checker_applies(&ResourceLifecycleChecker, &f));
    }

    // ── D3: always advisory-coexisting, never config-gated ───────────────────

    #[test]
    fn advisory_coexisting_is_always_true() {
        assert!(ResourceLifecycleChecker.advisory_coexisting());
    }

    #[test]
    fn rule_id_stays_in_llm_advisory_set_regardless_of_config() {
        let ids = crate::arch_checker::all_checker_rule_ids();
        assert!(!ids.contains(RULE_RESOURCE_LIFECYCLE), "{ids:?}");
        // But the checker IS registered and answers the rule id.
        let all_ids: HashSet<&str> =
            crate::arch_checker::all_checkers().iter().flat_map(|c| c.rule_ids().iter().copied()).collect();
        assert!(all_ids.contains(RULE_RESOURCE_LIFECYCLE), "{all_ids:?}");
    }

    #[test]
    fn checker_is_registered() {
        let ids: HashSet<&str> =
            crate::arch_checker::all_checkers().iter().flat_map(|c| c.rule_ids().iter().copied()).collect();
        assert!(ids.contains(RULE_RESOURCE_LIFECYCLE), "{ids:?}");
    }
}
