#!/usr/bin/env python3
"""Collect Camerata's build metrics with local tools only (no AI calls) and write them, each number
beside the exact command that produced it, to docs/metrics/<YYYY-MM>.md.

One command, from the repository root:

    python3 -I scripts/metrics/collect_build_metrics.py            # everything (hours: coverage, mutants, clean build)
    python3 -I scripts/metrics/collect_build_metrics.py --quick    # minutes: skips clippy, coverage, mutants, clean build

Sections: (1) test vs non-test lines per crate; (2) line and branch coverage (cargo-llvm-cov);
(3) cargo-mutants on the gate and rule-engine crates, jobs capped at 4; (4) Clippy warnings at the
enforced level and at the tracked frontier, unsafe blocks; (5) process numbers from git history,
GitHub and the saved run logs; (6) rule count and the share with a source; (7) the crate dependency
graph and a clean build time.

The heavy steps (clippy, coverage, mutants, clean build) refuse to start while a `camerata inspect`
process is running. A tool that is not installed is reported, never installed by this script.
Optional sibling folders for section 5 (relative to the repository's parent, overridable):
CAMERATA_INTERNAL_DIR (default ../camerata-internal), CAMERATA_EFF_DIR (default ../_workspace/camerata-eff).
"""

import argparse
import datetime as dt
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PARENT = ROOT.parent
INTERNAL = Path(os.environ.get("CAMERATA_INTERNAL_DIR", PARENT / "camerata-internal"))
EFF = Path(os.environ.get("CAMERATA_EFF_DIR", PARENT / "_workspace" / "camerata-eff"))
CARGO_ENV = {**os.environ, "CARGO_BUILD_JOBS": "4", "CARGO_INCREMENTAL": "0"}
MUTANT_PACKAGES = ["camerata-gateway", "camerata-rules"]
FRONTIER_FLAGS = ["-W", "clippy::pedantic", "-W", "clippy::expect_used", "-W", "clippy::panic"]
HEAVY = {"clippy", "coverage", "mutants", "clean-build"}
SECTIONS = ["lines", "clippy", "unsafe", "coverage", "mutants", "process", "rules", "graph", "clean-build"]


def run(cmd, timeout=None, env=None):
    """Run a command in the repo root; return (exit code, stdout, stderr, seconds)."""
    start = time.time()
    p = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, timeout=timeout, env=env)
    return p.returncode, p.stdout, p.stderr, time.time() - start


def shown(cmd):
    return " ".join(cmd)


def inspection_running():
    code, out, _, _ = run(["pgrep", "-fl", "camerata inspect"])
    return code == 0 and out.strip() != ""


def crates():
    return sorted(p for p in (ROOT / "crates").iterdir() if (p / "Cargo.toml").exists())


# (1) Test vs non-test lines ----------------------------------------------------------------------

TEST_FILE = re.compile(r"(^|/)(tests?|benches)(/|$)|(^|/)tests\.rs$|_tests\.rs$|(^|/)test_[^/]*\.rs$")
CFG_TEST = re.compile(r"^\s*#\[cfg\((all\()?test")
MOD_INLINE = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{")
MOD_DECL = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?mod\s+(\w+)\s*;")


def split_test_lines(path, rel, test_mod_files):
    """Count (test, non-test) lines of one file. Whole files count as test by path or because a
    `#[cfg(test)] mod x;` points at them; inline `#[cfg(test)] mod x { ... }` blocks count as test
    by brace depth (an approximation: braces inside strings are not special-cased)."""
    lines = path.read_text(errors="replace").splitlines()
    if TEST_FILE.search(rel) or path in test_mod_files:
        return len(lines), 0
    test = 0
    i = 0
    while i < len(lines):
        if CFG_TEST.match(lines[i]):
            j = i + 1
            while j < len(lines) and j <= i + 3 and lines[j].strip().startswith("#["):
                j += 1
            if j < len(lines) and MOD_INLINE.match(lines[j]):
                depth, k = 0, j
                while k < len(lines):
                    depth += lines[k].count("{") - lines[k].count("}")
                    k += 1
                    if depth <= 0:
                        break
                test += k - i
                i = k
                continue
        i += 1
    return test, len(lines) - test


def test_module_files(rs_files):
    """Files declared as `#[cfg(test)] mod x;` (x.rs or x/mod.rs beside the declaring file, or
    under its module folder)."""
    marked = set()
    for f in rs_files:
        lines = f.read_text(errors="replace").splitlines()
        for i, line in enumerate(lines[:-1]):
            if CFG_TEST.match(line):
                m = MOD_DECL.match(lines[i + 1])
                if m:
                    name = m.group(3)
                    base = f.parent if f.name in ("mod.rs", "lib.rs", "main.rs") else f.parent / f.stem
                    for c in (f.parent / f"{name}.rs", base / f"{name}.rs", base / name / "mod.rs"):
                        if c.exists():
                            marked.add(c)
                    folder = base / name
                    if folder.is_dir():
                        marked.update(folder.rglob("*.rs"))
    return marked


def section_lines():
    rows, tot_t, tot_n = [], 0, 0
    for c in crates():
        files = [f for f in c.rglob("*.rs") if "target" not in f.parts]
        marked = test_module_files(files)
        t = n = 0
        for f in files:
            a, b = split_test_lines(f, str(f.relative_to(c)), marked)
            t, n = t + a, n + b
        rows.append((c.name, t, n))
        tot_t, tot_n = tot_t + t, tot_n + n
    tools = ROOT / "tools"
    out = ["## 1. Test lines vs non-test lines", "",
           "Command: `python3 -I scripts/metrics/collect_build_metrics.py` (section `lines`): every `.rs` file under "
           "`crates/*/`. A file counts as test when it sits under `tests/` or `benches/`, is named `tests.rs`, "
           "`*_tests.rs` or `test_*.rs`, or is declared by `#[cfg(test)] mod x;`; inline `#[cfg(test)] mod x { }` "
           "blocks count by brace depth. Blank and comment lines are included in both columns.", "",
           "| Crate | Test lines | Non-test lines | Test share |", "|---|---:|---:|---:|"]
    for name, t, n in rows:
        out.append(f"| {name} | {t:,} | {n:,} | {t / max(t + n, 1):.0%} |")
    out.append(f"| **Total ({len(rows)} crates)** | **{tot_t:,}** | **{tot_n:,}** | **{tot_t / max(tot_t + tot_n, 1):.0%}** |")
    if tools.exists():
        extra = sum(len(f.read_text(errors="replace").splitlines()) for f in tools.rglob("*.rs") if "target" not in f.parts)
        out.append("")
        out.append(f"Maintainer tools under `tools/` (not in the table): {extra:,} lines.")
    return out


# (4) Clippy and unsafe ---------------------------------------------------------------------------

def clippy_count(extra):
    cmd = ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--message-format=json", "--"] + extra
    code, out, err, secs = run(cmd, env=CARGO_ENV)
    warnings = errors = 0
    for line in out.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("reason") != "compiler-message":
            continue
        level = m["message"]["level"]
        msg = m["message"]["message"]
        if msg.startswith("aborting due to") or re.match(r"^\d+ warnings? emitted", msg):
            continue
        if level == "warning":
            warnings += 1
        elif level == "error":
            errors += 1
    return warnings, errors, shown(cmd), secs, code


def section_clippy():
    w, e, cmd, secs, code = clippy_count([])
    fw, fe, fcmd, fsecs, _ = clippy_count(FRONTIER_FLAGS)
    return ["## 4a. Clippy", "",
            "| Level | Warnings | Errors | Command |", "|---|---:|---:|---|",
            f"| Enforced (workspace lints; CI adds `-D warnings`, so any warning here fails CI) | {w:,} | {e:,} | `{cmd}` |",
            f"| Tracked frontier (the non-blocking CI job's flags) | {fw:,} | {fe:,} | `{fcmd}` |", "",
            f"Exit code of the enforced run: {code}. Times: {secs:.0f} s and {fsecs:.0f} s (warm cache)."]


UNSAFE = re.compile(r"\bunsafe\s*(\{|fn\b|impl\b|trait\b)")


def section_unsafe():
    hits = []
    for f in (ROOT / "crates").rglob("*.rs"):
        if "target" in f.parts:
            continue
        for n, line in enumerate(f.read_text(errors="replace").splitlines(), 1):
            code_part = line.split("//", 1)[0]
            if UNSAFE.search(code_part):
                hits.append(f"{f.relative_to(ROOT)}:{n}")
    cargo = (ROOT / "Cargo.toml").read_text()
    forbid = 'unsafe_code = "forbid"' in cargo
    inherit = sum(1 for c in crates() if "[lints]" in (c / "Cargo.toml").read_text()
                  and "workspace = true" in (c / "Cargo.toml").read_text())
    out = ["## 4b. Unsafe code", "",
           f"- `unsafe` blocks, functions, impls or traits in `crates/` (comments excluded): **{len(hits)}**. "
           "Command: `python3 -I scripts/metrics/collect_build_metrics.py` (section `unsafe`), regex "
           r"`\bunsafe\s*(\{|fn|impl|trait)` over every `.rs` file.",
           f"- Workspace lint `unsafe_code = \"forbid\"` in `Cargo.toml`: {'yes' if forbid else 'no'}; "
           f"crates inheriting the workspace lints: {inherit} of {len(crates())}."]
    out += [f"  - {h}" for h in hits[:20]]
    return out


# (2) Coverage ------------------------------------------------------------------------------------

def section_coverage():
    if run(["cargo", "llvm-cov", "--version"])[0] != 0:
        return ["## 2. Coverage", "",
                "Not measured: cargo-llvm-cov is not installed. Install it with "
                "`cargo install cargo-llvm-cov` and `rustup component add llvm-tools-preview`, then rerun."]
    report = ROOT / "target" / "metrics-llvm-cov.json"
    base = ["cargo", "llvm-cov", "--workspace", "--locked", "--json", "--summary-only", "--output-path", str(report)]
    cmd = base + ["--branch"]
    code, _, err, secs = run(cmd, env=CARGO_ENV)
    branch = code == 0
    if not branch:
        cmd = base
        code, _, err, secs = run(cmd, env=CARGO_ENV)
    if code != 0:
        return ["## 2. Coverage", "", f"The coverage run failed (exit {code}). Command: `{shown(cmd)}`", "",
                "```", err[-2000:], "```"]
    data = json.loads(report.read_text())["data"][0]
    per = {}
    for f in data["files"]:
        m = re.search(r"/crates/([^/]+)/", f["filename"])
        if not m:
            continue
        s = f["summary"]
        acc = per.setdefault(m.group(1), [0, 0, 0, 0])
        acc[0] += s["lines"]["covered"]
        acc[1] += s["lines"]["count"]
        acc[2] += s.get("branches", {}).get("covered", 0)
        acc[3] += s.get("branches", {}).get("count", 0)
    t = data["totals"]
    out = ["## 2. Coverage", "", f"Command: `{shown(cmd)}` ({secs / 60:.0f} min). "
           + ("Branch coverage included." if branch else "Branch coverage needs a nightly toolchain (`--branch` was refused), so only line coverage is shown."),
           "", "| Crate | Lines covered | Line coverage | Branch coverage |", "|---|---:|---:|---:|"]
    for name, (lc, ln, bc, bn) in sorted(per.items()):
        out.append(f"| {name} | {lc:,} / {ln:,} | {lc / max(ln, 1):.1%} | {f'{bc / bn:.1%}' if branch and bn else 'n/a'} |")
    out.append(f"| **Total** | {t['lines']['covered']:,} / {t['lines']['count']:,} | **{t['lines']['percent'] / 100:.1%}** | "
               f"{(str(round(t['branches']['percent'], 1)) + '%') if branch and t.get('branches', {}).get('count') else 'n/a'} |")
    return out


# (3) Mutants -------------------------------------------------------------------------------------

def section_mutants(packages):
    if run(["cargo", "mutants", "--version"])[0] != 0:
        return ["## 3. Mutation testing", "", "Not measured: cargo-mutants is not installed (`cargo install cargo-mutants`)."]
    out = ["## 3. Mutation testing", "",
           "| Crate | Mutants | Caught | Missed | Timeout | Unviable | Score (caught / (caught + missed + timeout)) | Command |",
           "|---|---:|---:|---:|---:|---:|---:|---|"]
    for pkg in packages:
        outdir = ROOT / "target" / f"metrics-mutants-{pkg}"
        cmd = ["cargo", "mutants", "--package", pkg, "--jobs", "4", "--timeout-multiplier", "3", "--output", str(outdir)]
        _, _, _, secs = run(cmd, env=CARGO_ENV)
        res = outdir / "mutants.out" / "outcomes.json"
        if not res.exists():
            out.append(f"| {pkg} | run failed (no outcomes.json) | | | | | | `{shown(cmd)}` |")
            continue
        o = json.loads(res.read_text())
        caught, missed = o.get("caught", 0), o.get("missed", 0)
        timeout, unviable = o.get("timeout", 0), o.get("unviable", 0)
        total = o.get("total_mutants", caught + missed + timeout + unviable)
        score = caught / max(caught + missed + timeout, 1)
        out.append(f"| {pkg} | {total} | {caught} | {missed} | {timeout} | {unviable} | {score:.1%} | `{shown(cmd)}` ({secs / 3600:.1f} h) |")
    return out


# (5) Process numbers -----------------------------------------------------------------------------

def git(args):
    return run(["git"] + args)[1]


def section_process():
    out = ["## 5. Process numbers", "",
           "| Number | Value | Command or source |", "|---|---:|---|"]
    repo = run(["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"])[1].strip()
    code, prs, _, _ = run(["gh", "pr", "list", "--state", "merged", "--limit", "2000", "--json", "number"])
    out.append(f"| Merged pull requests | {len(json.loads(prs)) if code == 0 else 'gh unavailable'} | "
               f"`gh pr list --state merged --limit 2000 --json number` ({repo}) |")
    code, runs, _, _ = run(["gh", "run", "list", "--workflow", "ci.yml", "--status", "failure", "--limit", "2000", "--json", "databaseId"])
    code2, allruns, _, _ = run(["gh", "run", "list", "--workflow", "ci.yml", "--limit", "2000", "--json", "databaseId"])
    if code == 0 and code2 == 0:
        out.append(f"| CI runs that ended in failure (of all CI runs) | {len(json.loads(runs))} of {len(json.loads(allruns))} | "
                   "`gh run list --workflow ci.yml --status failure --limit 2000`; counts every cause, including stretches "
                   "when CI was red for a reason outside the change (a new supply-chain advisory, say) |")
    gate_logs = sorted(EFF.glob("gate/*.log")) if EFF.exists() else []
    refused = 0
    for g in gate_logs:
        text = g.read_text(errors="replace")
        if re.search(r"_EXIT=[1-9]", text) or "WRONG_BRANCH" in text:
            refused += 1
    out.append(f"| Local full-gate runs that refused to push (of saved runs) | {refused} of {len(gate_logs)} | "
               f"saved logs `{EFF.name}/gate/*.log` with any `*_EXIT` other than 0 |")
    red = sorted(EFF.glob("gate/*red*.txt")) if EFF.exists() else []
    out.append(f"| Red-first runs saved to disk (a floor) | {len(red)} | `{EFF.name}/gate/*red*.txt`; most red runs were recorded "
               "only in evidence packs, which are not all kept |")
    msgs = git(["log", "--format=%s%n%b", "--all"])
    waves = set(int(w) for w in re.findall(r"(?i)\bwave\s+(\d+)\b", msgs))
    if INTERNAL.exists():
        for f in INTERNAL.rglob("fable_review_wave*"):
            m = re.search(r"wave(\d+)", f.name)
            if m:
                waves.add(int(m.group(1)))
    out.append(f"| Fable review waves (distinct wave numbers found) | {len(waves)} | `git log --all --format='%s%n%b'` "
               f"matched `wave N`, plus `fable_review_wave*` files in `{INTERNAL.name}` |")
    fable_commits = git(["log", "--all", "--no-merges", "-i", "--grep=fable", "--format=%h"]).split()
    out.append(f"| Non-merge commits citing a Fable review (proxy for findings fixed) | {len(fable_commits)} | "
               "`git log --all --no-merges -i --grep=fable --format=%h` |")
    commits = git(["rev-list", "--count", "HEAD"]).strip()
    out.append(f"| Commits on this branch | {commits} | `git rev-list --count HEAD` |")
    out.append("")
    out.append("Not recorded anywhere, so not reported: rejections by Camerata's own agent gate during its build "
               "(the builders were Claude Code sessions, not agents behind the Camerata gateway).")
    return out


# (6) Rules ---------------------------------------------------------------------------------------

def section_rules():
    files = sorted((ROOT / "crates" / "rules" / "principles").rglob("*.toml"))
    with_src = total_src = 0
    principles = ROOT / "crates" / "rules" / "principles"
    domains = {p.name for p in principles.iterdir() if p.is_dir()}
    for f in files:
        text = f.read_text(errors="replace")
        n = len(re.findall(r"^\[\[sources\]\]", text, re.M))
        total_src += n
        with_src += n > 0
    return ["## 6. Rules", "",
            f"- Rules: **{len(files)}** across {len(domains)} domains (top-level folders); with at least one `[[sources]]` entry: "
            f"**{with_src}** ({with_src / max(len(files), 1):.1%}); sources in all: {total_src}.",
            "- Command: `python3 -I scripts/metrics/collect_build_metrics.py` (section `rules`): every `.toml` under "
            "`crates/rules/principles/`, counting `[[sources]]` tables; domains are the top-level folders."]


# (7) Graph and clean build -----------------------------------------------------------------------

def section_graph():
    cmd = ["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"]
    meta = json.loads(run(cmd)[1])
    names = {p["name"] for p in meta["packages"]}
    edges = {p["name"]: sorted({d["name"] for d in p["dependencies"] if d["name"] in names and d.get("kind") in (None, "build")})
             for p in meta["packages"]}
    dev = sum(1 for p in meta["packages"] for d in p["dependencies"] if d["name"] in names and d.get("kind") == "dev")
    cycles, state = [], {}

    def visit(n, stack):
        state[n] = 1
        for m in edges[n]:
            if state.get(m) == 1:
                cycles.append(stack[stack.index(m):] + [m])
            elif not state.get(m):
                visit(m, stack + [m])
        state[n] = 2

    for n in edges:
        if not state.get(n):
            visit(n, [n])
    memo = {}

    def depth(n):
        if n not in memo:
            memo[n] = 1 + max((depth(m) for m in edges[n]), default=0)
        return memo[n]

    deepest = max(edges, key=depth)
    chain, n = [deepest], deepest
    while edges[n]:
        n = max(edges[n], key=depth)
        chain.append(n)
    return ["## 7a. Crate dependency graph", "",
            f"- Workspace packages: {len(names)}; internal dependency edges (normal and build): {sum(len(v) for v in edges.values())}; "
            f"dev-only internal edges: {dev}.",
            f"- Cycles: **{len(cycles)}**" + (": " + "; ".join(" -> ".join(c) for c in cycles) if cycles else "."),
            f"- Longest dependency chain: **{depth(deepest)}** packages: {' -> '.join(chain)}.",
            f"- Command: `{shown(cmd)}`, internal edges walked in `collect_build_metrics.py` (section `graph`)."]


def section_clean_build():
    target = PARENT / "_workspace" / "metrics-clean-target"
    clean = ["cargo", "clean", "--target-dir", str(target)]
    build = ["cargo", "build", "--workspace", "--locked", "--target-dir", str(target)]
    run(clean)
    code, _, err, secs = run(build, env=CARGO_ENV)
    return ["## 7b. Clean build", "",
            f"- Clean debug build of the workspace: **{secs / 60:.1f} min** (exit {code}), with `CARGO_BUILD_JOBS=4`, on "
            f"{os.uname().sysname} {os.uname().machine}, {os.cpu_count()} cores.",
            f"- Commands: `{shown(clean)}` then `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 {shown(build)}`.",
            f"- The build folder is kept at `../_workspace/{target.name}` for the next run to clean."]


# Main --------------------------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--quick", action="store_true", help="skip clippy, coverage, mutants and the clean build")
    ap.add_argument("--only", help="comma-separated sections: " + ",".join(SECTIONS))
    ap.add_argument("--skip", help="comma-separated sections to skip")
    ap.add_argument("--mutants-packages", default=",".join(MUTANT_PACKAGES))
    ap.add_argument("--out", help="output file (default docs/metrics/<YYYY-MM>.md)")
    a = ap.parse_args()
    chosen = a.only.split(",") if a.only else list(SECTIONS)
    if a.quick:
        chosen = [s for s in chosen if s not in HEAVY]
    if a.skip:
        chosen = [s for s in chosen if s not in a.skip.split(",")]
    if any(s in HEAVY for s in chosen) and inspection_running():
        sys.exit("A `camerata inspect` process is running: the heavy sections (clippy, coverage, mutants, clean build) "
                 "never run during an inspection. Rerun later or pass --quick.")
    now = dt.datetime.now().astimezone()
    out_path = Path(a.out) if a.out else ROOT / "docs" / "metrics" / f"{now:%Y-%m}.md"
    head = git(["rev-parse", "HEAD"]).strip()
    branch = git(["rev-parse", "--abbrev-ref", "HEAD"]).strip()
    doc = [f"# Build metrics, {now:%Y-%m}", "",
           f"Collected {now:%Y-%m-%d %H:%M %Z} at commit `{head}` on `{branch}` by "
           "`python3 -I scripts/metrics/collect_build_metrics.py"
           + (" --quick" if a.quick else "") + (f" --only {a.only}" if a.only else "") + (f" --skip {a.skip}" if a.skip else "")
           + "`. Local tools only; no AI calls. Every number below names the command that produced it.", ""]
    order = {"lines": section_lines, "coverage": section_coverage,
             "mutants": lambda: section_mutants(a.mutants_packages.split(",")),
             "clippy": section_clippy, "unsafe": section_unsafe, "process": section_process,
             "rules": section_rules, "graph": section_graph, "clean-build": section_clean_build}
    for s in SECTIONS:
        if s in chosen:
            print(f"collecting {s} ...", flush=True)
            try:
                doc += order[s]() + [""]
            except Exception as e:  # a failed section is reported, never silently dropped
                doc += [f"## {s}", "", f"Section failed: `{type(e).__name__}: {e}`", ""]
        else:
            doc += [f"<!-- section `{s}` not collected this run -->", ""]
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(doc))
    print(f"wrote {out_path.relative_to(ROOT) if out_path.is_relative_to(ROOT) else out_path}")


if __name__ == "__main__":
    main()
