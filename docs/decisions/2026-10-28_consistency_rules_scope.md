# ADR: Scope of the consistency rules

**Date:** 2026-10-28
**Status:** Accepted; the rules and their checkers are built to these rulings.
**Deciders:** The project owner

Rules this record backs: `ARCH-ONE-IMPORT-PATH-1`, `ARCH-SHARED-CONSTANT-1`, `ARCH-DEAD-CODE-1`,
`ARCH-SINGLE-SOURCE-LOGIC-1`, `ARCH-NAMING-ONE-CONCEPT-1` and `UI-SHARED-PRIMITIVE-1`. Each of
them cites this record as the place where its scope is written down. The checkers are described in
`docs/design/2026-10-21_w29-detector-implementation.md`.

## Context

The consistency rules report code that has more than one home: a symbol with two import paths, a
domain value written out in several files, a decision implemented twice, a file nothing uses. A
check of this kind can always be widened until it reports more. Every widening costs trust, because
a client who meets a wrong finding stops reading the right ones. The question is where each rule
stops.

## Decision

The policy is the same for every rule: report only what the checker can show, and leave out what it
cannot show with a low rate of false reports. A narrower rule that is right is worth more than a
wider rule that is sometimes wrong. Widening happens one shape at a time, each with its own safe
twins (a nearby case the rule must stay silent about).

1. **One import path.** A symbol that outside code imports both from the file that defines it and
   from an index barrel that re-exports it is a violation. Index barrels are not exempt from the
   one-home rule. Files inside the barrel's own folder that import their siblings directly are
   not part of it.

2. **Dead code works on whole files in the first release.** A file counts as used when another
   file imports it, a framework or tool loads it by convention or a comment records how it is
   loaded. Unused individual exports become a separate future rule once the tooling can report
   them without false positives. The rule runs in the inspection and in CI. It does not run at the
   write-time gate, because a story may legitimately end with scaffolding or a file that is
   consumed from outside the repository, and a bounce there would teach agents to paste
   keep-file markers.

3. **Shared constants use a narrow definition of a domain value.** Only four shapes qualify: a
   lowercase name split by a hyphen, a colon or a slash, a route template, a multi-word upper-case
   token and a decimal rate in arithmetic. Rates need at least two significant digits, so `0.12`
   and `0.075` qualify while `0.9` and `0.001` do not. URL names passed to a reverse or redirect
   call are excluded. Copies are grouped within the nearest package root, so importing the
   constant is always possible. A value that has two or more named homes and no bare copy is not a
   finding. Other shapes are widened one at a time, each with its own safe twins.

4. **The repeated-code rules ship in the first release.** `ARCH-SINGLE-SOURCE-LOGIC-1`,
   `ARCH-NAMING-ONE-CONCEPT-1` and `UI-SHARED-PRIMITIVE-1` are built on one clone detector and
   share these narrowings, all of which favour silence over a doubtful finding:
   - None of the three emits a "clean" row, because the detector cannot prove that no repeated
     code exists.
   - The single-source rule escalates to high severity only when the copies disagree on a decision
     token (an operator, a number or a property inside the decision test). A difference in a
     string or a template is a trailing clause on a medium finding.
   - The composed-variant guard fires only when a helper call dominates the shared sequence: the
     call covers at least half of its tokens or every decision token lies inside the call's
     arguments.
   - Functions that render JSX are left out of the naming rule when the shared-primitive rule is
     armed, so the two rules never report the same code.

## Consequences

- A finding from these rules can be defended from the code alone, and the cases left out are
  listed here and in each rule's qualification text.
- Widening any of these rules is a new decision that adds a record, not an edit to this one.
