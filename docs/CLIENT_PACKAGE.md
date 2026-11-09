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
| `findings.json` | The same rows as the workbook's All problems sheet, wrapped with provenance, summary counts, coverage, the grouping tree and the fixes. A project of several repos adds a `patterns_across_services` key | Scripts, CI and other tools |
| `decisions.json` | Every decision record the report was reviewed with. The workbook's Reviewer decisions sheet is the same list | Tools; the architect |
| `csv/tickets.csv` | One ticket for each fix that still has places to change, ready to import into Jira, Linear or GitHub Issues | Engineers and project managers |
| `csv/all-findings.csv` and `json/all-findings.json` | Every problem, once. The same rows as All problems | Tools |
| `csv/deviations.csv` and `json/deviations.json` | The same rows as the Departures sheet | Tools |
| `csv/conventions.csv` and `json/conventions.json` | The same rows as the Your conventions sheet | Tools |
| `csv/not-assessed.csv` and `json/not-assessed.json` | The rules that could not be completed, which are also listed on the Not checked sheet | Tools |
| `csv/dependencies.csv` and `json/dependencies.json` | The same rows as the Dependencies sheet | Tools |
| `csv/false-positives.csv` and `json/false-positives.json` | The same rows as the Ruled out sheet | Tools |
| `csv/coverage.csv` and `json/coverage.json` | The same rows as the Rules checked sheet | Tools |
| `csv/rules-applied.csv` and `json/rules-applied.json` | For each rule with several accepted ways to do a thing, the way the inspection judged the code against, with the evidence | Tools |
| `manifest.json` | The list of the files in the ZIP, each with its purpose, its schema version and a checksum, and the facts of the inspection: the commit of each repo, the engine version, the rule library version and the models used | Anyone checking that the package is whole and unchanged |
| `README.txt` | What each file is and how to import the tickets | Whoever opens the ZIP first |

`<stem>` is `camerata-inspection-<repo>-<short commit>`, so the file names the repo and the commit it describes.

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

The inspection finds more problems than a person can read one by one. A group reviewer first reads each rule's problems together and splits them into fixes. It then ranks the places that most need a person's eyes. A budget decides how many of those places the architect reads in person. By default it is one place for every 1,000 counted lines of code, with a minimum of 20. The places above that line are the escalated places. Each one is read in person, and the workbook marks it "Reviewed in person".

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
