//! Tests for the piece planner (design section 1).

use super::pieces::{plan_pieces, units_for, Unit};

fn file(path: &str, bytes: usize) -> (String, String) {
    (path.to_string(), "x".repeat(bytes))
}

fn one_unit(files: &[(String, String)]) -> Vec<Unit> {
    units_for("acme/app", files)
}

#[test]
fn every_file_is_in_exactly_one_piece_and_the_counts_add_up() {
    let files: Vec<(String, String)> = (0..30)
        .map(|i| file(&format!("src/mod{}/f{i}.rs", i % 5), 1_000))
        .collect();
    let pieces = plan_pieces(&one_unit(&files), &files, 6_000, None);
    assert!(
        pieces.len() > 1,
        "30 files of 1,000 bytes over a 6,000 target must split"
    );
    let total: usize = pieces.iter().map(|p| p.files.len()).sum();
    assert_eq!(total, files.len());
    let mut seen = std::collections::BTreeSet::new();
    for p in &pieces {
        for f in &p.files {
            assert!(seen.insert(f.clone()), "{f} is in two pieces");
        }
    }
    assert_eq!(seen.len(), files.len());
}

#[test]
fn the_same_input_gives_the_same_pieces_and_keys() {
    let files: Vec<(String, String)> = (0..20)
        .map(|i| file(&format!("pkg{}/f{i}.py", i % 4), 700))
        .collect();
    let a = plan_pieces(&one_unit(&files), &files, 4_000, None);
    assert!(
        a.len() > 1,
        "20 files of 700 bytes over a 4,000 target must split"
    );
    let mut shuffled = files.clone();
    shuffled.reverse();
    let b = plan_pieces(&one_unit(&shuffled), &shuffled, 4_000, None);
    assert_eq!(a, b);
    let keys: std::collections::BTreeSet<&str> = a.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys.len(), a.len(), "keys are unique");
}

#[test]
fn a_3000_line_unit_is_one_piece() {
    // 3,000 lines of 40 bytes is 120,000 bytes, well under the 240,000 target.
    let body = "let value = compute(input, options);\n".repeat(3_000);
    let files = vec![
        ("src/a.rs".to_string(), body.clone()),
        ("src/b.rs".to_string(), "fn b() {}\n".to_string()),
    ];
    let pieces = plan_pieces(&one_unit(&files), &files, super::pieces::TARGET_BYTES, None);
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].files.len(), 2);
}

#[test]
fn a_unit_at_one_and_a_quarter_times_the_target_is_one_piece_and_one_byte_more_is_not() {
    let files = vec![file("a/one.rs", 6_000), file("b/two.rs", 6_500)];
    // 12,500 bytes against a target of 10,000 is exactly 1.25 times.
    assert_eq!(
        plan_pieces(&one_unit(&files), &files, 10_000, None).len(),
        1
    );
    let bigger = vec![file("a/one.rs", 6_000), file("b/two.rs", 6_501)];
    assert_eq!(
        plan_pieces(&one_unit(&bigger), &bigger, 10_000, None).len(),
        2
    );
}

#[test]
fn forcing_four_gives_at_least_four_pieces_on_a_unit_of_eight_files() {
    let files: Vec<(String, String)> = (0..8)
        .map(|i| file(&format!("src/f{i}.rs"), 500 + i * 37))
        .collect();
    let pieces = plan_pieces(
        &one_unit(&files),
        &files,
        super::pieces::TARGET_BYTES,
        Some(4),
    );
    assert!(pieces.len() >= 4, "got {} pieces", pieces.len());
    assert_eq!(pieces.iter().map(|p| p.files.len()).sum::<usize>(), 8);
}

#[test]
fn forcing_more_pieces_than_files_gives_one_piece_for_each_file() {
    let files: Vec<(String, String)> = (0..3).map(|i| file(&format!("f{i}.rs"), 100)).collect();
    let pieces = plan_pieces(
        &one_unit(&files),
        &files,
        super::pieces::TARGET_BYTES,
        Some(10),
    );
    assert_eq!(pieces.len(), 3);
}

#[test]
fn a_folder_is_not_cut_while_it_fits() {
    // Three folders of 3,000 bytes each against a target of 4,000: each folder fits, so every
    // piece boundary falls between folders.
    let mut files = Vec::new();
    for dir in ["api", "core", "web"] {
        for i in 0..3 {
            files.push(file(&format!("{dir}/f{i}.rs"), 1_000));
        }
    }
    let pieces = plan_pieces(&one_unit(&files), &files, 4_000, None);
    assert_eq!(pieces.len(), 3);
    for p in &pieces {
        let dirs: std::collections::BTreeSet<&str> = p
            .files
            .iter()
            .map(|f| f.split('/').next().unwrap_or(""))
            .collect();
        assert_eq!(dirs.len(), 1, "a folder was cut or merged: {:?}", p.files);
        assert_eq!(p.files.len(), 3);
    }
}

#[test]
fn a_folder_over_the_target_is_opened_and_a_file_over_the_target_stands_alone() {
    let files = vec![
        file("big/a.rs", 3_000),
        file("big/b.rs", 3_000),
        file("big/huge.rs", 9_000),
        file("small/c.rs", 500),
    ];
    let pieces = plan_pieces(&one_unit(&files), &files, 5_000, None);
    let huge = pieces
        .iter()
        .find(|p| p.files.iter().any(|f| f == "big/huge.rs"))
        .expect("huge file is in a piece");
    assert_eq!(huge.files, vec!["big/huge.rs".to_string()]);
    assert_eq!(pieces.iter().map(|p| p.files.len()).sum::<usize>(), 4);
}

#[test]
fn a_piece_never_crosses_a_unit() {
    let files: Vec<(String, String)> = vec![file("svc_a/x.rs", 100), file("svc_b/y.rs", 100)];
    let units = vec![
        Unit {
            name: "a".into(),
            files: vec!["svc_a/x.rs".into()],
        },
        Unit {
            name: "b".into(),
            files: vec!["svc_b/y.rs".into()],
        },
    ];
    let pieces = plan_pieces(&units, &files, super::pieces::TARGET_BYTES, None);
    assert_eq!(pieces.len(), 2);
    for p in &pieces {
        assert_eq!(p.files.len(), 1);
        assert!(p.key.starts_with(&format!("{}#", p.unit)));
    }
}

#[test]
fn units_for_returns_one_unit_for_the_repository() {
    let files = vec![file("b.rs", 1), file("a.rs", 1)];
    let units = units_for("acme/app", &files);
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].name, "acme/app");
    assert_eq!(units[0].files, vec!["a.rs".to_string(), "b.rs".to_string()]);
    assert!(units_for("acme/app", &[]).is_empty());
}

#[test]
fn a_synthetic_40_file_repository_unsplit_and_forced_to_4() {
    let files: Vec<(String, String)> = (0..40)
        .map(|i| {
            file(
                &format!("src/m{}/file{:02}.rs", i / 10, i),
                2_000 + (i % 7) * 150,
            )
        })
        .collect();
    let units = one_unit(&files);
    let unsplit = plan_pieces(&units, &files, super::pieces::TARGET_BYTES, None);
    let forced = plan_pieces(&units, &files, super::pieces::TARGET_BYTES, Some(4));
    println!("unsplit: {} piece(s)", unsplit.len());
    for p in &unsplit {
        println!("  {}  {} files", p.key, p.files.len());
    }
    println!("forced to 4: {} piece(s)", forced.len());
    for p in &forced {
        println!(
            "  {}  {} files  first {}  last {}",
            p.key,
            p.files.len(),
            p.files[0],
            p.files[p.files.len() - 1]
        );
    }
    assert_eq!(unsplit.len(), 1);
    assert_eq!(unsplit[0].files.len(), 40);
    assert!(forced.len() >= 4);
    assert_eq!(forced.iter().map(|p| p.files.len()).sum::<usize>(), 40);
}
