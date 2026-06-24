# Tech debt: derive rule `domain` from the folder; retire the hand-typed field + the `"*"` sentinel

> **Status: STAGED — not yet filed as a GitHub issue.**
> On the next "GitHub push", create this as a **sub-issue of the Tech Debt Epic (#70)** using the title + body below.

**Title:** Derive rule `domain` from the corpus folder path; relabel `"*"` → `"Universal"`

---

## Problem

Every rule TOML hand-declares `domain = "..."`, a **free string read from the file** (`crates/rules/src/lib.rs`: `domain: String` → `domain: raw.domain`). The loader (`collect_toml_paths_sync`) recursively reads every `.toml` and **ignores the folder name** — the folder is cosmetic; the domain comes from inside the file. So the field and the folder can diverge, silently.

Concrete failure that surfaced this: `ARCH-RESOURCE-LIFECYCLE-1` was placed in `principles/universal/` but declared `domain = "universal"`. Universal rules are keyed under the sentinel `"*"` (the bucket `select_for_domains` always includes via `r.domain == "*"`), so the rule **loaded but was filtered out of the proposed list** until the field was corrected to `"*"`. A hand-typed domain that must match an invisible convention is fragile.

Key observation: **for every rule except universal, the folder already equals the domain** — `rust/` → `"rust"`, `api-layer/` → `"api-layer"`, `rust/dioxus/` → `"rust:dioxus"`. The only divergence is `universal/` (folder) vs `"*"` (field). So the field is redundant with the path everywhere, and the one place it diverges is exactly where it broke.

## Proposed redesign

1. **Derive `domain` from the corpus-relative folder path** of the TOML (e.g. `principles/rust/dioxus/x.toml` → `"rust:dioxus"`, `principles/universal/x.toml` → `"universal"`). Drop the in-file `domain` field, or keep it optional as an explicit override only (and fail loudly if an override disagrees with the folder).
2. **Relabel the universal sentinel `"*"` → `"Universal"`** (human-readable, and it falls out of the `universal/` folder automatically). Update the always-include check in `select_for_domains` (`r.domain == "*"` → the new universal value) and anywhere `"*"` is special-cased.
3. **Review the remaining hardcoded domain→behavior maps** so a new `principles/<domain>/` folder is as close to "just works" as possible — `derive_allowed_paths` (domain → file glob, currently `match` on `"rust"`/`"ui"`/...) and the stack/language → domain selection. At minimum, a domain with no glob/selection mapping should fail loudly (logged), not be silently dropped.

## Acceptance

- Adding `principles/<domain>/x.toml` registers the rule under `<domain>` with no hand-typed field that can mismatch.
- The UI shows "Universal" instead of `"*"`.
- A mistyped/mismatched domain is impossible (derived) or fails loudly (override disagreement).

## Scope
`crates/rules/src/lib.rs` (loader + `select_for_domains` + `derive_allowed_paths`), every TOML's `domain` field (drop or make override-only), the universal rules' `"*"` → `"Universal"`, and any server-side stack→domain selection. Parent: **Tech Debt Epic #70**.
