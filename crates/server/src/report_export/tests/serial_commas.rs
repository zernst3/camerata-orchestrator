//! No serial comma in the prose the report writes itself: `a, b or c`, never `a, b, or c`. The
//! scan reads the string literals of the producers and the PDF template.

/// The text of every string literal in a Rust source's production code, with the line
/// continuations joined, and in a Typst template the whole text. Comments and the test module
/// are left out.
fn authored_prose(source: &str, is_template: bool) -> Vec<String> {
    // Only a test module ends the production code; `#[cfg(test)]` on a single item does not.
    let mut end = source.len();
    let mut from = 0;
    while let Some(at) = source[from..].find("#[cfg(test)]") {
        let at = from + at;
        let after = source[at + "#[cfg(test)]".len()..].trim_start();
        if after.starts_with("mod ") || after.starts_with("pub mod ") {
            end = at;
            break;
        }
        from = at + 1;
    }
    let source = &source[..end];
    if is_template {
        return vec![source
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")];
    }
    let code: String = source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut literals = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut literal = String::new();
        while let Some(n) = chars.next() {
            match n {
                '\\' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                        while chars.peek().is_some_and(|c| c.is_whitespace()) {
                            chars.next();
                        }
                    } else if let Some(escaped) = chars.next() {
                        literal.push(escaped);
                    }
                }
                '"' => break,
                _ => literal.push(n),
            }
        }
        literals.push(literal);
    }
    literals
}

/// An aside is a bare pair of words ("past and future") set off by commas inside a longer clause
/// ("Shared with everyone with access, past and future, and stays readable"), not an item of a
/// list. A longer item with its own conjunction ("identity and access management") is a list item,
/// and so is a pair after a short lead ("a, b and c, or d").
fn is_aside(lead: &str, item: &str) -> bool {
    let words: Vec<&str> = item.split_whitespace().collect();
    words.len() == 3 && matches!(words[1], "and" | "or") && lead.split_whitespace().count() >= 4
}

/// `a, b, and c` / `a, b, or c`: a comma before the last item of a list of three or more.
fn serial_commas(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for conjunction in [", and ", ", or ", ", nor "] {
        let mut from = 0;
        while let Some(at) = text[from..].find(conjunction) {
            let at = from + at;
            from = at + conjunction.len();
            let head = &text[..at];
            let clause_start = head
                .rfind(['.', ':', ';', '(', '\n', '"', '?', '!'])
                .map_or(0, |i| i + 1);
            let clause = &head[clause_start..];
            // A list has another comma before the last item, and the last item is short and holds
            // no conjunction of its own ("past and future" is an aside, not an item).
            let Some(previous) = clause.rfind(", ") else {
                continue;
            };
            let last_item = &clause[previous + 2..];
            if last_item.split_whitespace().count() <= 6
                && !is_aside(&clause[..previous], last_item)
            {
                found.push(format!("{}{conjunction}", clause.trim()));
            }
        }
    }
    found
}

/// Each second-person word in a prose, with the text around it (cut on character boundaries).
fn second_person_in(prose: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let mut from = 0;
    while from < prose.len() {
        let rest = &prose[from..];
        let Some(start) = rest.find(|c: char| c.is_alphanumeric()) else {
            break;
        };
        let word_len = rest[start..]
            .find(|c: char| !c.is_alphanumeric() && c != '\'')
            .unwrap_or(rest.len() - start);
        let (word_start, word_end) = (from + start, from + start + word_len);
        let word = &prose[word_start..word_end];
        if [
            "you", "your", "yours", "yourself", "you'll", "you're", "you've", "you'd",
        ]
        .contains(&word.to_lowercase().as_str())
        {
            let before = prose[..word_start]
                .char_indices()
                .rev()
                .nth(40)
                .map_or(0, |(i, _)| i);
            let after = prose[word_end..]
                .char_indices()
                .nth(40)
                .map_or(prose.len(), |(i, _)| word_end + i);
            hits.push(format!("...{}", &prose[before..after]));
        }
        from = word_end.max(from + start + 1);
    }
    hits
}

#[test]
fn the_second_person_scan_finds_every_form_and_cuts_on_a_character_boundary() {
    for text in [
        "you've seen it",
        "what you'd do",
        "If you act",
        "Your code",
        "yours",
    ] {
        assert_eq!(second_person_in(text).len(), 1, "{text}");
    }
    // Multi-byte characters around the hit must not panic the excerpt.
    let text = "ééééééééééééééééééééééééééééééééééééééééééééé you ééééééééééééééééééééééééééééééééééééééééééééé";
    assert_eq!(second_person_in(text).len(), 1);
    assert!(second_person_in("The client's code, not yours truly's, is read").len() == 1);
    assert!(second_person_in("a young youth").is_empty());
}

/// The workbook reason ("before you act") is exempt from the second-person scan only because the
/// PDF never prints it. If the template ever reads `why_waiting`, this fails and the exemption
/// has to go.
#[test]
fn the_pdf_template_does_not_read_why_waiting() {
    let template = include_str!("../../../templates/audit_report.typ");
    for field in ["why_waiting", "waiting_reason"] {
        assert!(
            !template.contains(field),
            "the template reads {field}: remove the second-person exemption for the waiting \
             reason in authored_pdf_prose_has_no_second_person first"
        );
    }
}

/// The PDF speaks of the client in the third person: no "you" or "your" in the prose the report
/// writes for it, outside a sheet name the vocabulary owns ("Your conventions").
#[test]
fn authored_pdf_prose_has_no_second_person() {
    let sources: [(&str, &str, bool); 12] = [
        (
            "report_export.rs",
            include_str!("../../report_export.rs"),
            false,
        ),
        (
            "citations_fixes.rs",
            include_str!("../citations_fixes.rs"),
            false,
        ),
        (
            "classification.rs",
            include_str!("../classification.rs"),
            false,
        ),
        ("marking.rs", include_str!("../marking.rs"), false),
        ("json_types.rs", include_str!("../json_types.rs"), false),
        (
            "coverage_gaps.rs",
            include_str!("../coverage_gaps.rs"),
            false,
        ),
        ("fix_text.rs", include_str!("../fix_text.rs"), false),
        ("row_facts.rs", include_str!("../row_facts.rs"), false),
        ("tree_view.rs", include_str!("../tree_view.rs"), false),
        ("prose_checks.rs", include_str!("../prose_checks.rs"), false),
        ("scan_tools.rs", include_str!("../../scan_tools.rs"), false),
        (
            "audit_report.typ",
            include_str!("../../../templates/audit_report.typ"),
            true,
        ),
    ];
    let mut offenders = Vec::new();
    for (name, source, is_template) in sources {
        for prose in authored_prose(source, is_template) {
            // The workbook's "Why it is waiting" column speaks in the product's voice ("before
            // you act"); it is not PDF prose.
            if prose.contains("a reviewer must rule on it before you act") {
                continue;
            }
            let prose = prose.replace(crate::xlsx_export::vocabulary::YOUR_CONVENTIONS, " ");
            for hit in second_person_in(&prose) {
                offenders.push(format!("{name}: {hit}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "second person in the prose written for the PDF (write about the client in the third \
         person):\n{}",
        offenders.join("\n")
    );
}

/// Every source file whose string literals reach a client (the PDF, the workbook, the README).
fn prose_sources() -> [(&'static str, &'static str, bool); 23] {
    [
        ("scan.rs", include_str!("../../routes/scan.rs"), false),
        ("ownership.rs", include_str!("../../ownership.rs"), false),
        ("scan_tools.rs", include_str!("../../scan_tools.rs"), false),
        (
            "xlsx_export.rs",
            include_str!("../../xlsx_export.rs"),
            false,
        ),
        ("row_facts.rs", include_str!("../row_facts.rs"), false),
        ("tree_view.rs", include_str!("../tree_view.rs"), false),
        ("overlay.rs", include_str!("../overlay.rs"), false),
        ("prose_checks.rs", include_str!("../prose_checks.rs"), false),
        (
            "review_sheets.rs",
            include_str!("../../xlsx_export/review_sheets.rs"),
            false,
        ),
        (
            "subgroups.rs",
            include_str!("../../xlsx_export/subgroups.rs"),
            false,
        ),
        (
            "tables.rs",
            include_str!("../../xlsx_export/tables.rs"),
            false,
        ),
        (
            "triage_log.rs",
            include_str!("../../xlsx_export/triage_log.rs"),
            false,
        ),
        (
            "report_export.rs",
            include_str!("../../report_export.rs"),
            false,
        ),
        (
            "citations_fixes.rs",
            include_str!("../citations_fixes.rs"),
            false,
        ),
        (
            "classification.rs",
            include_str!("../classification.rs"),
            false,
        ),
        ("marking.rs", include_str!("../marking.rs"), false),
        ("json_types.rs", include_str!("../json_types.rs"), false),
        (
            "coverage_gaps.rs",
            include_str!("../coverage_gaps.rs"),
            false,
        ),
        ("fix_text.rs", include_str!("../fix_text.rs"), false),
        (
            "vocabulary.rs",
            include_str!("../../xlsx_export/vocabulary.rs"),
            false,
        ),
        (
            "sheets.rs",
            include_str!("../../xlsx_export/sheets.rs"),
            false,
        ),
        (
            "tickets.rs",
            include_str!("../../xlsx_export/tickets.rs"),
            false,
        ),
        (
            "audit_report.typ",
            include_str!("../../../templates/audit_report.typ"),
            true,
        ),
    ]
}

/// No em or en dash in the literals of those files, except `scan_tools.rs`: it also holds the
/// operator's own diagnostics (log and panic text), which no client reads, and its client-facing
/// strings are held by the release gate's text-style check on the finished files.
#[test]
fn authored_prose_has_no_em_or_en_dash() {
    let mut offenders = Vec::new();
    for (name, source, is_template) in prose_sources() {
        if name == "scan_tools.rs" {
            continue;
        }
        for prose in authored_prose(source, is_template) {
            if let Some(at) = prose.find(['\u{2014}', '\u{2013}']) {
                let from = prose[..at]
                    .char_indices()
                    .rev()
                    .nth(30)
                    .map_or(0, |(i, _)| i);
                let to = prose[at..]
                    .char_indices()
                    .nth(30)
                    .map_or(prose.len(), |(i, _)| at + i);
                offenders.push(format!("{name}: ...{}...", &prose[from..to]));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "em or en dash in authored prose (write a colon, a comma or two sentences):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn authored_prose_has_no_serial_comma() {
    let sources = prose_sources();
    let mut offenders = Vec::new();
    for (name, source, is_template) in sources {
        for prose in authored_prose(source, is_template) {
            for hit in serial_commas(&prose) {
                offenders.push(format!("{name}: {hit}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "serial commas in authored prose (write `a, b or c`):\n{}",
        offenders.join("\n")
    );
}

/// The README is scanned as the package prints it (the text in `lib.rs` is spliced from the
/// vocabulary, so the source alone would not show it), for a project of one repo and of two.
#[test]
fn the_client_readme_has_no_serial_comma() {
    use crate::{product_export_readme, ManifestProvenance, ProvenanceRepo};
    for repos in [&["acme/portal"][..], &["acme/portal", "acme/billing"][..]] {
        let provenance = ManifestProvenance {
            repos: repos
                .iter()
                .map(|name| ProvenanceRepo {
                    name: (*name).to_string(),
                    ..ProvenanceRepo::default()
                })
                .collect(),
            ..ManifestProvenance::default()
        };
        let readme = product_export_readme("acme-inspection", &provenance, false);
        assert!(
            serial_commas(&readme).is_empty(),
            "{:?}",
            serial_commas(&readme)
        );
    }
}

/// The executive summary sentence as the PDF prints it, in both review states and with and
/// without the dependency clause: the source scan sees only the literal fragments around the
/// numbers, so the printed sentence is scanned too.
#[test]
fn the_executive_summary_sentence_has_no_serial_comma() {
    use crate::report_export::{counts_prose, ReportCounts, ReviewState};
    for state in [ReviewState::Raw, ReviewState::Reviewed] {
        for (held, info, ruled_out, advisories) in [(14, 6, 0, 0), (1, 1, 1, 3), (0, 0, 7, 0)] {
            let mut c = ReportCounts::default();
            c.other.files_scanned = 120;
            c.problems.total = 12 + held + info;
            c.problems.for_action = 12;
            c.problems.needs_decision = held;
            c.problems.information_only = info;
            c.rows.ruled_out = ruled_out;
            c.rows.dependency = advisories;
            for sentence in [
                counts_prose::narrative(&c, state),
                counts_prose::methodology_line(&c, state),
                counts_prose::next_steps(&c, false, state, false),
                counts_prose::grid_left_out(&c),
            ] {
                assert!(
                    serial_commas(&sentence).is_empty(),
                    "{state:?}: {sentence}\n{:?}",
                    serial_commas(&sentence)
                );
            }
        }
    }
}

#[test]
fn the_serial_comma_scan_finds_a_list_whose_last_item_is_a_short_clause() {
    assert!(!serial_commas(
        "1 needs a decision, 6 are information only, and 0 were ruled out automatically."
    )
    .is_empty());
    assert!(
        serial_commas("1 needs a decision, 6 are information only and 0 were ruled out.")
            .is_empty()
    );
}

/// A `#[cfg(test)]` on a single item (a helper only the tests call) does not end the production
/// code of the file: only a test module does.
#[test]
fn the_scan_reads_past_a_cfg_test_item_and_stops_at_the_test_module() {
    let source = "const A: &str = \"x\";\n#[cfg(test)]\nfn helper() {}\nconst B: &str = \"a, b, and c\";\n#[cfg(test)]\nmod tests {\n    const C: &str = \"d, e, and f\";\n}\n";
    let literals = authored_prose(source, false);
    assert!(literals.iter().any(|l| l == "a, b, and c"), "{literals:?}");
    assert!(!literals.iter().any(|l| l == "d, e, and f"), "{literals:?}");
}

#[test]
fn the_serial_comma_scan_finds_a_list_and_leaves_a_clause_alone() {
    assert!(!serial_commas("a private key, keystore, or real .env").is_empty());
    assert!(!serial_commas("read, modify, or delete rows").is_empty());
    assert!(serial_commas("a private key, keystore or real .env").is_empty());
    assert!(serial_commas("It ships on Friday, and we keep it").is_empty());
    // A long lead-in does not hide a list, and an aside with its own "and" is not a list.
    assert!(!serial_commas(
        "It is advisory and not a certification, warranty, or guarantee of security."
    )
    .is_empty());
    assert!(!serial_commas("a, b and c, or d").is_empty());
    assert!(!serial_commas(
        "No review of organizational controls, identity and access management, or infrastructure."
    )
    .is_empty());
    assert!(
        serial_commas("Shared with everyone with access, past and future, and stays readable")
            .is_empty()
    );
}
