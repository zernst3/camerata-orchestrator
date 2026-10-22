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
                | "spec"
                | "__fixtures__"
        )
    }) {
        return true;
    }
    file.rfind('.').is_some_and(|dot| {
        let stem = &file[..dot];
        [".test", ".spec", "_test", "_spec", ".cy", ".stories"]
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
    fn jasmine_rspec_and_fixture_conventions_are_test_paths() {
        for path in [
            "spec/models/invoice_spec.js",
            "spec/support/jasmine.json",
            "spec/helpers/customMatchers.js",
            "packages/x/spec/a.js",
            "src/__fixtures__/data.ts",
            "app/models/user_spec.rb",
            "src/invoice_spec.js",
            "tests/a.ts",
            "src/a.test.ts",
        ] {
            assert!(is_test_path(path), "{path}");
        }
    }

    #[test]
    fn near_misses_are_not_test_paths() {
        for path in [
            "src/specs_helper.ts",
            "src/inspector.ts",
            "src/spectrum/colors.ts",
            "src/fixtures_loader.ts",
            "src/aspec.ts",
            "src/lib/invoice.js",
        ] {
            assert!(!is_test_path(path), "{path}");
        }
    }
}
