//! No serial comma in the prose the report writes itself: `a, b or c`, never `a, b, or c`. The
//! scan reads the string literals of the producers and the PDF template.

/// The text of every string literal in a Rust source's production code, with the line
/// continuations joined, and in a Typst template the whole text. Comments and the test module
/// are left out.
fn authored_prose(source: &str, is_template: bool) -> Vec<String> {
    let source = source.split("#[cfg(test)]").next().unwrap_or(source);
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
            if last_item.split_whitespace().count() <= 3
                && !last_item.contains(" and ")
                && !last_item.contains(" or ")
            {
                found.push(format!("{}{conjunction}", clause.trim()));
            }
        }
    }
    found
}

#[test]
fn authored_prose_has_no_serial_comma() {
    let sources: [(&str, &str, bool); 8] = [
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
            "audit_report.typ",
            include_str!("../../../templates/audit_report.typ"),
            true,
        ),
    ];
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
    assert!(
        serial_commas("Shared with everyone with access, past and future, and stays readable")
            .is_empty()
    );
}
