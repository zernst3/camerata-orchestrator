# The client package

This page says what a Codebase Inspection delivers: every file in the package, what each one holds, who it is for and how to read it. It describes what the export writes today, and it was checked against a real package built from a fixture repository. How the inspection runs is in [`PLATFORM.md`](../PLATFORM.md). What the operator does with the export is in [`USER_GUIDE.md`](USER_GUIDE.md) section 11a. What leaves the operator's machine is in [`DATA_HANDLING.md`](../DATA_HANDLING.md).

## What the package is and who reads it

The package is one ZIP. It holds a PDF, an Excel workbook, files for tools and a README. Every file comes from the same inspection of the same commit, and the release gate refuses the export if the files disagree with each other.

Three kinds of reader use it, each with a different file:

- **The client's leadership** reads the PDF. It is a summary of about 15 pages, written for a person who will not open a spreadsheet. It does not carry every row, and that is by design.
- **The client's engineers** work from the workbook and the files for tools. The workbook holds every problem once, the fixes in the order to do them and the reason for each. The CSV and JSON files and the ticket file hold the same rows for scripts and issue trackers.
- **The architect who reviewed the report** reads the reviewer sheets at the end of the workbook, which show how problems were grouped and what each reviewer decision was.

The words are the same in every file: a **fix** is one cause and one change that clears a set of problems, a **place** is one file and line where a problem was seen, a **problem** is what was found at a place and a **rule** is one thing the inspection looked for. The glossary at the end of this page defines every term, and the workbook carries the same glossary.

## Every file in the package

| File | What it holds | For |
|---|---|---|
| `<stem>.pdf` | The summary report: cover, executive summary, the three things to do first, the scorecard by area, the most serious problems, one line for every other fix, conventions, what is healthy, dependencies, next steps and the method with its limits | Leadership |
| `<stem>-findings.xlsx` | The workbook: every problem, every fix, every rule checked and everything not checked. Sheets are listed below | Engineers, then the architect |
| `findings.json` | Every row that was not ruled out, of every kind: the rows behind All problems, Your conventions and Not checked together, before All problems shows each problem once. Wrapped with provenance, summary counts, coverage, the grouping tree and the fixes. A project of several repos adds a `patterns_across_services` key | Scripts, CI and other tools |
| `decisions.json` | Every decision record the report was reviewed with. The workbook's Reviewer decisions sheet is the same list | Tools; the architect |
| `csv/tickets.csv` | One ticket for each fix that still has places to change, ready to import into Jira, Linear or GitHub Issues | Engineers and project managers |
| `csv/all-findings.csv` and `json/all-findings.json` | The same rows as `findings.json`, as a flat table | Tools |
| `csv/deviations.csv` and `json/deviations.json` | The same rows as the Departures sheet | Tools |
| `csv/conventions.csv` and `json/conventions.json` | The same rows as the Your conventions sheet | Tools |
| `csv/not-assessed.csv` and `json/not-assessed.json` | The rules that could not be completed, which are also listed on the Not checked sheet | Tools |
| `csv/dependencies.csv` and `json/dependencies.json` | The same rows as the Dependencies sheet | Tools |
| `csv/false-positives.csv` and `json/false-positives.json` | The same rows as the Ruled out sheet | Tools |
| `csv/coverage.csv` and `json/coverage.json` | The same rows as the Rules checked sheet | Tools |
| `csv/rules-applied.csv` and `json/rules-applied.json` | For each rule with several accepted ways to do a thing, the way the inspection judged the code against, with the evidence | Tools |
| `manifest.json` | The list of the files in the ZIP, each with its purpose, its schema version and a checksum, and the facts of the inspection: the commit of each repo, the engine version, the rule library version and the models used | Anyone checking that the package is whole and unchanged |
| `README.txt` | What each file is and how to import the tickets | Whoever opens the ZIP first |

`<stem>` is `camerata-inspection-<repo>-<short commit>`, so the file names the repo and the commit it describes. A project of several repos is named `camerata-inspection-<project>-<N>-repos`.

The files for tools keep their own field names (`severity`, `rule_id`, `site_id`, `fix_id` and so on) and their own wire values, so that scripts written against them keep working. Those names are for tools. A person reads the workbook, where every column carries the vocabulary of this page. Each machine file is built from the same rows as the sheet it matches, so they cannot disagree.

### The ticket file

`csv/tickets.csv` has the columns Title, Description, Priority, Labels, Estimate, Fix id, Places and Place list. Places counts the places of the ticket. Place list names the first 50, then points to the All problems sheet. Fix id is the Fix id of that sheet, so it is the value to filter the workbook on. Priority is Highest, High, Medium or Low. Estimate is text, not a duration. A place ruled out or accepted as a risk is on no ticket. `README.txt` gives the column mapping for each of the three trackers.

## The workbook, sheet by sheet

The sheets are in reading order and in four colour groups. Every table has its header on row 1 with filters, a frozen header row and a link back to Start here. The columns at the right edge, headed "Technical (ids for tooling)", hold ids for tools and are collapsed until opened.

**Start (dark tabs)**

1. **Start here.** The title, who it was prepared for and by, a linked list of every sheet with its row count, the scorecard by area and the facts of the inspection.
2. **Glossary.** What every word and column in the workbook means.

**What to fix (red tabs)**

3. **Fix plan.** One row per fix, in the order to do them, with what is wrong, why it matters, what to do and an example written for the client's own code.
4. **All problems.** Every problem once, at the place it was seen, with every rule it breaks, what to do there and the code seen. Sorted by fix order so the places of one fix sit together.
5. **Needs a decision.** The problems a reviewer must rule on before the client acts, each with why it is waiting.
6. **Your conventions.** What the code already does consistently, and where it has no single habit.
7. **Departures.** Where the code departs from a convention it mostly follows.
8. **Dependencies.** Known vulnerabilities in the packages the code depends on, with the version that fixes each.

**What we checked (gray tabs)**

9. **Rules checked.** Every rule that ran and what it found, clean rules included.
10. **Patterns across services.** Only in a project of several repos: how each service follows each rule. An observation with no severity, in no count.
11. **Ruled out.** What was looked at and is not a problem, with the reason.
12. **Not checked.** The rules that did not run or could not finish, the files that were not read and the dependency manifests that could not be checked, so nothing is dropped silently.

**For reviewers (light tabs)**

13. **All source rows.** The raw record: every row the engine produced, including the source rows behind a merged problem. It is the one sheet where a problem can be listed more than once.
14. **Reviewer decisions.** Every decision a reviewer recorded.
15. **How problems were grouped.** How problems were gathered into areas, rules, rule groups and fixes, with the counts at each step.

Every other sheet shows a problem once.

## The PDF, section by section

The PDF is a summary. Its outline has fifteen sections, in this order:

1. **Cover.** Client, repos, commit, date and the counts of problems by severity.
2. **Contents.** A clickable outline.
3. **Executive summary.** What was inspected, how many problems there are and how many are for action, need a decision or are information only.
4. **If you only do three things this week.** One entry for each of the first fixes.
5. **Area scorecard.** For each area, the problems by severity, the rules checked and the rules clean.
6. **Severity by effort matrix.** How many problems sit at each severity and effort.
7. **The most serious problems.** The places reviewed in person, worst first, at most eight: what is wrong, where, what to do and the effort. A line points to the Fix plan sheet for the rest.
8. **The other fixes.** One line for each remaining fix, grouped by area, with its places, severity, effort and fix order. At most 60 lines, then a line pointing to the Fix plan sheet.
9. **Your conventions.** The conventions followed consistently and the departures from them.
10. **What is healthy.** The rules that came back clean, at most 25.
11. **Dependencies.** The most serious dependency advisories, at most five, with the rest on the Dependencies sheet.
12. **Next steps.**
13. **Where the details are.** Which sheet answers which question, using the exact tab names.
14. **How this was done and its limits.** The method and what it cannot show.
15. **Disclaimer.** The report is an assessment and not a certification, warranty or guarantee.

A report that no human reviewer has signed off carries a draft banner on every page.

## How the escalation line works

The inspection finds more problems than a person can read one by one. A group reviewer first reads each rule's problems together and splits them into fixes. It then ranks the places that most need a person's eyes. A budget decides how many of those places the architect reads in person. By default it is one place for every 1,000 counted lines of code, with a minimum of 20. The places above that line are the escalated places. A critical place whose reason is one of the severe kinds (unauthenticated reach, a secret exposed, data loss, cross-tenant access, authorization bypass, injection, unverified evidence) is escalated even past the budget. Each one is read in person, and the workbook marks it "Reviewed in person".

A place below the line is not left unchecked. The engine checked it against the code, and it is on the workbook like any other problem, with the Reviewed in person cell blank. The PDF's "The most serious problems" section prints the escalated places, worst first. The budget itself is the operator's setting and is not printed in a client file.

## What is not in the package

- A certification, warranty or guarantee. The report is an advisory assessment of the repository at one commit and says so.
- Anything about the live system. The inspection reads the code, not the production environment.
- Fixes already applied. The package says what to change and where. It changes no code.
- The reviewer's working notes, the group reviewer's reasons and ranks, the escalation budget, the time spent on review and any cost figure. These are the operator's and are left out of every client file.
- Problems the engine or the reviewer ruled out, apart from a count and the Ruled out sheet that gives the reason for each.

## Glossary

The glossary below is generated from the same vocabulary the workbook's Glossary sheet prints, so the two cannot differ. Because the workbook speaks to the client, its definitions address the reader as "you". To change a definition, change the vocabulary and regenerate this block.

To regenerate it: `UPDATE_CLIENT_PACKAGE_DOC=1 cargo test -p camerata-server client_package_doc_glossary_block`.

<!-- glossary:start -->

| Term | Plain meaning | Where it appears |
|---|---|---|
| Fix | One cause and one change that clears a whole set of problems, wherever they appear. | Fix plan; the Fix column on All problems |
| Rule group | The places of one rule that share one cause. A fix is made of one or more rule groups. | How problems were grouped |
| Open | A fix or a place with at least one rule no reviewer has ruled on yet. The count says how many rules are decided. | The Decision so far column of Fix plan and All problems |
| Mixed | Shown as Mixed, see rules: the rules of a fix were decided differently, so the Rules column and the reviewer decisions say which way each went. | The Decision so far column of Fix plan and All problems |
| Shares places with | The other fixes that list the same place without one containing the other, with how many places they share. | Fix plan |
| Place | One file and line, or one whole file, where a problem was seen. | File and Line columns; every Places count |
| Problem | What was found at a place. A problem lists every rule it breaks. | All problems; the Problem column |
| Area | The part of your system a rule belongs to, such as security or testing. | Area columns; the scorecard on Start here |
| Rule | One thing we looked for. | Rules checked; the Rule and Rules columns |
| Rule id | The short code that names a rule. Tools read it, and you can quote it when you ask us about a rule. | The technical columns at the right of every table |
| Fix order | When to do a fix: First, Next or Later. First is every critical and high fix and the quick medium ones. Next is the rest of the medium fixes. Later is the low ones. | Fix plan; All problems; Needs a decision |
| First | Fix order: do this fix before the others. Every critical and high fix, and the medium fixes that are quick. | Fix plan; All problems |
| Next | Fix order: do this fix after the First ones. The rest of the medium fixes. | Fix plan; All problems |
| Later | Fix order: plan this fix after the Next ones. The low fixes. | Fix plan; All problems |
| Severity | How bad the problem is if it is left alone: Critical, High, Medium or Low. Info marks an observation that is not a defect, such as a convention noted. | Every table of problems; the scorecard on Start here |
| Info | A severity for an observation that is not a defect, such as a convention noted. It counts in the Low column of the scorecard. | The Severity column; the scorecard on Start here |
| Effort | A rough size of the change for one place: low, medium or high. | Fix plan; All problems |
| Estimated hours | The range of hours the effort stands for, for one place. | All problems |
| Confidence | How sure the review is that the problem is real: High, Low or Disputed. Low means the review was unsure; Disputed means the review judged it probably not a real problem. A Low problem waits on Needs a decision. | All problems; Needs a decision |
| Needs a decision | A reviewer must rule on the problem before you act on it. | The Needs a decision sheet; the column of that name on All problems |
| Ruled out | Looked at and not a problem. The reason is on the Ruled out sheet. | The Ruled out sheet |
| Convention | A habit your code already follows consistently. | Your conventions; Departures |
| Departure | A place where your code departs from a convention it mostly follows. | The Departures sheet |
| Source | Who found it: Deterministic, Model-reviewed or Tool followed by the tool's name. | All problems; Needs a decision |
| Deterministic | Found by a check that cites an authoritative published standard and gives the same answer every time. | The Source column; Rules checked |
| Model-reviewed | Found by a model reading your code. It may cite a related standard. | The Source column; Rules checked |
| Tool | Found by an outside analysis tool that ran during the inspection. | The Source column; Rules checked |
| Related standard | A published standard matched to a model-reviewed problem by topic. It is not a rule we checked. | The Standard cited column |
| Standard cited | The published standard, such as an OWASP or CWE entry, behind the rule. | All problems; Rules checked |
| Reviewed in person | Yes when a person read the problem in your code. Blank means the engine checked it against your files without a person reading it. | All problems; Dependencies |
| Engine verified | Checked by the engine against your files, without a person reading it. | How problems were grouped |
| Example from your code | A fix written for the code at one place in your repo. | Fix plan |
| Standard fix | The rule's own guidance, not written for your code. | The Fix type column |
| Tailored fix | A fix written for your code that passed our checks. | The Fix type column |
| Fix type | Whether the fix text is a Tailored fix or a Standard fix. | All problems; Departures |
| Kind | What a row is: a defect, a departure, a convention, no convention, a rule that could not be completed or a dependency advisory. Tools read it; people use the sheet it sits on. | The technical columns |
| Carried from an earlier inspection | A problem carried forward unchanged because only changed files were inspected again. The column holds the earlier commit. | The Carried from column |
| Carried from | The commit of the earlier inspection a problem was carried forward from. Blank when it was found this time. | The technical columns of All problems |
| Dependency advisory | A published vulnerability in a package you depend on. | The Dependencies sheet |
| Status | The state the engine holds a problem in, for example suppressed by an earlier baseline. For tools. | The technical columns of All problems |
| Place id | A short code that names one place. It is the same in every file of this package. | The technical columns |
| Fix id | A code that names one fix. It matches a row of the Fix plan. | The technical columns |
| Rule ids | Every rule id behind a problem: its own rule and any rule that flagged the same place. | The technical columns of All problems |
| Review level | How closely the problem was reviewed, in the engine's own words. | The technical columns |
| Merged into | The Place id of the problem a reviewer merged this one into. | The technical columns; Reviewer decisions |
| Reviewer edited | Yes when a reviewer changed the text of this row. | The technical columns |
| Edit ids | The ids of the reviewer's edits to this row. | The technical columns |
| Standard links | The web addresses of the standards cited. | The technical columns of All problems |
| Group id | A code that names one group of problems: an area, a rule, a rule group or a fix. | The technical columns |
| Parent group id | The Group id of the group this one sits under. | How problems were grouped |
| Inherited from | The id of the group decision a decision for one place was written by. | Reviewer decisions |
| Order | The place of a fix in the plan: 1 is the first to do. | Fix plan |
| Places | How many places a fix, a problem or a group covers. | Fix plan; All problems; How problems were grouped; the tickets file |
| Place list | The places a ticket names, one to a line: the file and line of each. Only in the tickets file. | The tickets file |
| Rules | The rules a fix or a problem breaks, by name. | Fix plan; All problems |
| Areas | The areas a fix touches. | Fix plan |
| Folders | The folders that hold most of a fix's places, with how many places each holds. | Fix plan |
| Reviewed by | How closely the places of a fix were reviewed. | Fix plan |
| Go to places | A link to the first place of a fix on All problems. | Fix plan |
| What is wrong | The cause the places of a fix share. | Fix plan |
| Why it matters | What the problem puts at risk. | Fix plan |
| What to do | The change that makes the fix. | Fix plan; Departures; Dependencies |
| What to do here | The change to make at this place. | All problems; Needs a decision |
| File | The file a problem was seen in. | Every table of problems |
| Line | The line in the file, or whole file when the problem is the file itself. | Every table of problems |
| Repo | The repository a place is in. | Every table of problems |
| Code seen | The lines of your code the problem was seen in. | All problems |
| Decision so far | What a reviewer has already decided about the problem. | All problems; Needs a decision |
| Why it is waiting | Why a reviewer must rule on the problem first. | Needs a decision |
| Also found at | Other places the same problem was found, up to a limit. All of them are on All source rows. | All problems; Your conventions |
| Why the severity changed | Why the severity is not the one the problem was first rated at. | All problems |
| What we checked | The thing we looked for, in words. | Your conventions; Patterns across services |
| What your code does | The convention your code follows, or the habits we saw when it follows none. | Your conventions |
| How consistently | Whether your code follows the convention everywhere we looked. | Your conventions |
| Places looked at | How many places we looked at to see the convention. | Your conventions |
| More detail | The longer explanation behind a row. | All problems; Your conventions; Not checked |
| Places departing | How many places depart from the convention. | Departures |
| Places checked | How many places we checked against the convention. | Departures |
| Worst severity | The highest severity among the departing places. | Departures |
| Where | The places that depart, up to a limit. All of them are on All source rows. | Departures |
| Package | The software package an advisory is about. | Dependencies |
| Advisory | The published description of the vulnerability. | Dependencies |
| Fixed in | The first version of the package that has the patch. | Dependencies |
| Result | What a rule found: how many places, or clean. | Rules checked |
| How we checked | Whether a rule is Deterministic, Model-reviewed or a Tool. | Rules checked |
| Alternative applied | For a rule with several accepted ways to do a thing, the way we judged your code against. | Rules checked |
| Evidence for it | The file and line that showed which alternative your code uses. | Rules checked |
| Go to fix | A link to the fix for the rule's problems, or to the problems when no fix covers them. | Rules checked |
| Ruled out by | Who ruled the problem out: the Reviewer or the Engine. | Ruled out |
| Reason | Why, in words. | Ruled out; Reviewer decisions |
| Kind of gap | What sort of thing was not checked. | Not checked |
| What | The rule, file or tool that was not checked. | Not checked |
| Why | Why it was not checked. | Not checked |
| Row role | Whether a source row is the Primary row of a problem or a row behind it: a collapsed source, or another place the same problem was found. | All source rows |
| Found how many ways | How many source rows share this Place id. | All source rows |
| Represented by | A link to the row that stands for this source row on the other sheets. | All source rows |
| Shown on | The sheet that holds the row standing for this source row. | All source rows |
| Decision id | A code that names one recorded decision. | Reviewer decisions |
| Scope | Whether a decision was made on one place or on a group of places. | Reviewer decisions |
| State | What the decision was: confirmed, ignored, ruled out, merged and so on. | Reviewer decisions |
| Places covered | How many places a group decision covers. | Reviewer decisions |
| Reason recorded | Yes when the reviewer wrote a reason down. | Reviewer decisions |
| Revoked | Yes when the decision was later undone. It stays in the record. | Reviewer decisions |
| Group | A group of problems: an area, a rule, a rule group or a fix. | How problems were grouped |
| Within | What kind of group it is: Area, Rule, Rule group or Fix. | How problems were grouped |
| Under | The group this one sits under. | How problems were grouped |
| Cause | Why the places of a fix belong together. | How problems were grouped |
| Summary | A short account of the group. | How problems were grouped |
| Reviewed with others like it | Places a reviewer ruled on as part of a group of places that share one cause. | How problems were grouped |
| Term | The word being defined. | Glossary |
| Plain meaning | What the word means in this package. | Glossary |
| Where you meet it | The sheets and columns where the word shows up. | Glossary |
| Sheet | A tab of this workbook. | Start here |
| What it is | One line on what a sheet holds. | Start here |
| Rows | How many rows a sheet holds. | Start here |
| Back to Start here | A link at the top of every sheet that returns you to the Start here page. | Every sheet |
| Start here | The first page: what this file is, a link to every sheet, the scorecard and the facts of the inspection. | The first tab |
| Glossary | This sheet: what every word and column means. | The second tab |
| Fix plan | One row for each fix, in the order to do them. | The What to fix tabs |
| All problems | Every problem once, with the place it was seen and what to do there. | The What to fix tabs |
| Your conventions | What your code already does consistently, and where it has no single habit. | The What to fix tabs |
| Departures | Where your code departs from a convention it mostly follows. | The What to fix tabs |
| Dependencies | Known vulnerabilities in the packages you depend on. | The What to fix tabs |
| Rules checked | Every rule we ran and what it found. | The What we checked tabs |
| Patterns across services | How each service follows each rule, patterns and differences alike: violated in some places, clean, not applicable or not reviewed. It is an observation, so it carries no severity. | The What we checked tabs, for projects of several repos |
| Pattern | How one service follows one rule: violated in some places, clean, not applicable or not reviewed. A difference between services is a pattern too. | Patterns across services |
| Not checked | What we could not or did not check: rules that could not finish, rules that did not run, files we did not read. | The What we checked tabs |
| All source rows | The raw record: every row the engine produced, including the collapsed source rows behind a merged problem. This is the one sheet where a problem can be listed more than once. | The For reviewers tabs |
| Reviewer decisions | Every decision a reviewer recorded, with the reason where it is shared with you. | The For reviewers tabs |
| How problems were grouped | How problems were gathered into areas, rules, rule groups and fixes, with the counts at each step. | The For reviewers tabs |
| Technical (ids for tooling) | The grey columns at the right of every table, closed by default. Open them with the plus sign above the columns. They hold ids and codes for tools; you do not need them to read the sheet. | The right edge of every table |

<!-- glossary:end -->
