# Inspection walkthrough (V1, Zach 2026-10-27)

A guided mode in the desktop app: a small popup points at the real control on screen, says in one
or two sentences what it does, and moves on as the user goes. V1 covers the Inspection only, from
an empty app to an exported package. Audience: Zach inspecting a repo from scratch after time away,
and anyone using Camerata later.

## Turning it on

- **"Guided inspection" switch** on the screen where an inspection starts. On means the walkthrough
  runs alongside the real flow; off hides it. The switch is remembered per user.
- **First launch** with no projects: the app offers it once ("Want a guided first inspection?").
  Declining is remembered; the switch stays available.
- **Restart anytime** from the Help menu and the command palette ("Start walkthrough").

## What a step looks like

- A popup anchored to the control (arrow pointing at it, the control lightly highlighted), a title,
  one or two plain sentences, "Step 4 of 12", Back / Next / Exit.
- Most steps advance on their own when the user does the thing (clicks Run, picks a project).
  Next is for steps that only explain.
- If the control is not on screen (wrong page, collapsed panel), the popup says how to get there
  ("Open Settings, then Chat backend") instead of pointing at nothing.
- The walkthrough never clicks, types or decides anything itself, and it obeys the same gates as
  the rest of the review screen (a reason prompt, the viewer and modals take precedence). Esc
  exits.

## Steps (V1, Inspection only)

1. Create or open a project.
2. Add the client repository (read-only access; where the copy lives; it is deleted after
   delivery).
3. Client name and inspection setup (it fills the report's confidentiality line).
4. Model backend: API for client work, and where the key is saved (the Keychain via Settings).
5. Rules: which stack rules apply and why; leave the defaults unless there is a reason.
6. Count and estimate (`camerata count` numbers: what the quote is priced on; the cost estimate
   and the spending cap).
7. Run the inspection; what the progress view shows; it can run unattended.
8. Failed or not-run passes: what the warning means and that the export says so.
9. The review queue: the four decisions and their keys, reason prompts, Undo.
10. Speed tools: stacked review of a group, the code viewer, the command palette (Cmd+K).
11. Review time and counts (internal only, never in the export).
12. Export the package: what each file is for; the release checks that run first.

The step text lives in ONE data file (ids, target, title, body, "how to get there", advance
trigger), not scattered through components, so wording is edited in one place and goes through
the same no-em-dash and release-text checks as other UI strings.

## Enforcement (the part that keeps it from rotting)

- Every control a step points at carries a stable `data-tour` id. A test renders each screen and
  fails if any step's target id is missing, so renaming or removing a button breaks the build
  instead of silently stranding the walkthrough.
- A test walks all steps in order against the test harness and checks each advance trigger fires.
- Text tests: no em-dashes, length cap per step (two sentences).

## Out of scope for V1

Walkthroughs for Rule Hub authoring, governance, remediation and settings; videos; analytics.
V2 can add more tours on the same data format.

## Build notes

Lives in the UI crates (pure step logic and the step table in `camerata-ui-core`, the popup host in
`camerata-ui`). Build after viewer 7d merges (same files). Accessibility: focus moves into the
popup, it is announced, and focus returns to the highlighted control when the popup closes.
