//! The one order rule (single counts D, round 1): the owner's table of severity by effort.

use super::*;

/// Critical is always First. High is First at low effort or with no estimate (owner ruling
/// 2026-11-16), Next at medium or high effort. Medium and low are Later. A reviewer's do-now and
/// do-later decisions override.
#[test]
fn the_order_table_is_exactly_the_owners() {
    use Disposition::{TechDebtLater, TechDebtNow, Unresolved};
    let efforts = [None, Some("low"), Some("medium"), Some("high")];
    let expect = |severity: &str, row: [&str; 4]| {
        for (effort, want) in efforts.iter().zip(row) {
            assert_eq!(
                matrix_bucket(Unresolved, severity, *effort),
                want,
                "{severity} with effort {effort:?}"
            );
        }
    };
    expect("critical", ["do_now"; 4]);
    expect("high", ["do_now", "do_now", "do_next", "do_next"]);
    expect("medium", ["plan"; 4]);
    expect("low", ["plan"; 4]);
    for effort in efforts {
        assert_eq!(matrix_bucket(TechDebtNow, "low", effort), "do_now");
        assert_eq!(matrix_bucket(TechDebtLater, "critical", effort), "plan");
    }
}

/// Every `.rs` file under `dir`, as (path, text).
fn sources_under(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir)
        .expect("the source directory reads")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            sources_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((path.display().to_string(), text));
            }
        }
    }
}

/// Nothing outside the one function derives an order: the second rule (`sequence_of`,
/// `Sequence::of`) is gone, and only `review_tree/places.rs` builds a place class for action.
#[test]
fn no_code_outside_the_one_function_derives_an_order() {
    let mut sources = Vec::new();
    sources_under(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    assert!(
        sources.len() > 100,
        "anti-vacuity: the scan read the source"
    );
    let gone = [
        "fn sequence_of",
        "sequence_of(",
        "Sequence::of",
        "enum Sequence",
    ];
    let mut offenders = Vec::new();
    for (path, text) in &sources {
        if path.ends_with("order_rule_tests.rs") {
            continue;
        }
        for needle in gone {
            if text.contains(needle) {
                offenders.push(format!("{path} has `{needle}`"));
            }
        }
        let is_test = path.contains("test");
        if !is_test
            && !path.ends_with("review_tree/places.rs")
            && text.contains("ForAction(FixOrder::")
        {
            offenders.push(format!("{path} builds an order of its own"));
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}
