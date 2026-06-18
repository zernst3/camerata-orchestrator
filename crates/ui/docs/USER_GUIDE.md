# Camerata — User Guide

This is the product user guide the in-app assistant uses to answer "how do I do X in Camerata?"
Keep it accurate to the shipped UI; it is included into the binary at build time.

## What Camerata is
A local-first desktop app that governs AI-assisted development. You point it at your existing
local repositories, it proposes a rules ruleset for each, applies that governance, and (when
you want) audits the code against it. Repos live on your machine; Camerata stores only configs
and pointers (as JSON in your OS app-data dir), never your code.

## Projects
- A **project** is the container for everything: its repos, ruleset, and onboarded state.
- From the **Projects** home you can **Create** a project (name it), **Open** one, **Import** a
  project config, or **Export** the open project.
- **Export** writes a single, path-free `camerata-project-<name>.json` containing just that
  project (its repos as `owner/repo`, the ruleset, and which repos are onboarded). It does NOT
  include local paths or your workspace settings.
- **Import** upserts a project config into your local projects. If you already have a project
  with the same name, Camerata warns you before overwriting it. After import, the project's
  repos won't have local paths yet — resolve them in the Rules view (see "Repo paths").

## Onboarding a repo (the main flow)
Open the **Onboard Repos** view. The steps:
1. **Add repos** to the project (they must be local checkouts).
2. **Scan** — Camerata detects each repo's stack (languages, frameworks, IaC like Terraform,
   CI like GitHub Actions) and proposes a starter ruleset, per repo.
3. **Pick rules** — each repo has its own recommended-rules table (use the repo single-select to
   switch repos). Rules are selected per repo; project-level rules apply to every repo. Click a
   rule to read its decision, options, the default, and each option's rationale.
4. **Apply rules** — writes the governance files (AGENTS.md / CONVENTIONS.md / CI gate /
   baseline) onto a `camerata/onboard-governance` branch in each repo's local clone AND pushes
   that branch to origin. **No pull request is opened** — edit freely, then **Open governance
   PR** as a separate step when you're ready. Applying marks the repo **onboarded**.
5. **Audit (optional)** — you can finish onboarding without it. Run it to surface existing
   violations to triage. Each repo is scanned only against its own selected rules (plus the
   always-on security floor: hardcoded secrets, raw-SQL concatenation, secrets in URLs).
6. **Apply mechanical rules to CI** — the final step (after triage) wires the selected
   mechanical rules into the repo's existing CI as enforced lint gates.

## Triaging findings (after an audit)
Findings land in three tables you switch between: **Unresolved**, **Ignored**, **Tech debt**.
- Select findings and **Ignore (with reason)** or **Save as tech debt**; they move tables.
- You can move findings between any of the three tables freely.
- "Needs review" findings carry a flag + a specific reason (often over-engineering / YAGNI on a
  small codebase) shown in its own column — these are flagged for a reason, beyond the trivial
  sense that everything needs review.
- In the Tech-debt table, mark each item **resolve later** or **resolve now**, then **Process**:
  "later" items become a tracked ticket; "now" items go to the dev engine.

## Repo paths (resolution + health)
Camerata is local-first: each repo resolves to a local folder on your machine. The **Rules**
view shows a health check — any repo that doesn't point at a valid local git checkout (common
right after importing a project) is flagged with a warning and a per-repo **Resolve…** button
that lets you browse to the repo's folder. These local paths are machine-specific and are never
included in an export.

## Auto-save
Your in-progress onboarding (scan, audit, per-repo rule selection, triage dispositions) is saved
continuously, so you can quit and reopen without re-scanning. A fresh scan starts a new session.
(A crash mid-scan can't be recovered — the scan just re-runs.)

## The other views
- **Development Surface** — import stories and run governed development: the human↔AI back-and-
  forth, tagging people into stories for clarification, and the review loop.
- **Rules** — manage a project's ruleset after onboarding (per-repo and project-level), see the
  repo-path health check, and re-emit governance.
- **Routines** — schedule governed runs.

## Common questions
- *"How do I onboard without running the audit?"* Apply the rules in the Onboard view; the audit
  is optional and onboarding completes on apply.
- *"Why is there no PR after I applied rules?"* By design — Apply creates + pushes the branch
  only; click **Open governance PR** to open the PR.
- *"I imported a project and the repos show broken paths."* Expected — local paths aren't
  exported. Use **Resolve…** in the Rules view to point each repo at its local folder.
