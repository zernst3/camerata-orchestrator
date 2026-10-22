//! Shared path conventions for architectural checkers: test/fixture/mock/e2e/story path
//! detection and committed tsc emit detection. Originally written for the
//! ARCH-ONE-IMPORT-PATH-1 checker; the upcoming dead-code and shared-constant checkers
//! (W29 spec) reuse them.

use std::collections::HashSet;

/// Test, fixture, mock, end-to-end and story locations, which reach internals directly by
/// design. Mirrors the gateway's `is_test_or_fixture_path` (`camerata-checks` cannot depend on
/// the gateway crate, and no helper exists here) and adds the JS/TS ecosystem's mock, e2e,
/// Cypress and Storybook conventions.
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut segs: Vec<&str> = lower.split('/').collect();
    let Some(file) = segs.pop() else {
        return false;
    };
    if segs.iter().any(|s| {
        matches!(
            *s,
            "tests"
                | "test"
                | "__tests__"
                | "testdata"
                | "fixtures"
                | "benches"
                | "__mocks__"
                | "e2e"
                | "cypress"
                | "stories"
        )
    }) {
        return true;
    }
    file.rfind('.').is_some_and(|dot| {
        let stem = &file[..dot];
        // `tests.py` / `test.py` / `test_x.py` are test modules by the Python and Django
        // conventions (pytest's `test_*.py`, Django's `tests.py`).
        stem == "tests"
            || stem == "test"
            || stem.starts_with("test_")
            || [".test", ".spec", "_test", ".cy", ".stories"]
                .iter()
                .any(|suffix| stem.ends_with(suffix))
    })
}

/// A committed `X.js` that sits beside `X.ts` or `X.tsx` in the same folder is build output
/// (tsc emit), a copy of the source rather than a second module. It must not count as an
/// importer or as a module, or the copy's imports would register as extra paths.
pub fn emitted_js_paths(files: &[(String, String)]) -> HashSet<&str> {
    let all: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    files
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| {
            p.strip_suffix(".js").is_some_and(|stem| {
                all.contains(format!("{stem}.ts").as_str())
                    || all.contains(format!("{stem}.tsx").as_str())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_test_module_names_are_test_paths() {
        for p in [
            "app/tests.py",
            "app/test.py",
            "app/test_x.py",
            "orders/test_client.py",
            "src/tests.ts",
            "src/test_helpers.rs",
            "app/Tests.py",
        ] {
            assert!(is_test_path(p), "{p}");
        }
    }

    #[test]
    fn names_that_only_contain_test_are_not_test_paths() {
        for p in [
            "app/testing.py",
            "app/contest.py",
            "app/attest.py",
            "app/latest.py",
            "app/testimony.py",
            "app/x_test_data.py",
            "app/protest_x.py",
        ] {
            // `x_test_data` and the others end in neither `_test` nor start with `test_`.
            assert!(!is_test_path(p), "{p}");
        }
    }
}
