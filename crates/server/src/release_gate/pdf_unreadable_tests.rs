//! A client PDF the gate cannot read is refused, never passed: a text check that silently skips
//! the file it cannot open is no check.

use super::tests::package;
use super::*;

/// Bytes that look like a real compiled PDF (a header, a body, a trailer) and are not one.
fn a_real_looking_pdf_that_cannot_be_read() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend(std::iter::repeat(b'x').take(4000));
    bytes.extend_from_slice(b"\n%%EOF\n");
    bytes
}

#[test]
fn an_unreadable_real_pdf_is_a_problem_with_a_clear_reason() {
    let problems =
        text_style::pdf_problems("report.pdf", &a_real_looking_pdf_that_cannot_be_read());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].file, "report.pdf");
    assert!(
        problems[0].message.contains("could not be read"),
        "{}",
        problems[0].message
    );
}

#[test]
fn a_stand_in_and_a_file_that_is_not_a_pdf_are_not_this_checks_to_judge() {
    // The stand-in the assembly zips before the PDF is compiled, and the dataset-set check owns
    // "this member is not a PDF".
    assert!(text_style::pdf_problems("r.pdf", b"%PDF-1.7\n% not compiled yet\n").is_empty());
    assert!(text_style::pdf_problems("r.pdf", b"%PDF-1.7 stand-in for the gate tests").is_empty());
    assert!(text_style::pdf_problems("r.pdf", b"not a pdf at all").is_empty());
}

#[test]
fn a_client_export_with_an_unreadable_pdf_is_refused() {
    let mut pkg = package();
    pkg.pdf = a_real_looking_pdf_that_cannot_be_read();
    let failure = match release(pkg) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("an unreadable PDF must not be released"),
    };
    assert!(
        failure
            .report
            .checks
            .iter()
            .filter(|o| o.check == CheckId::ClientTextStyle)
            .flat_map(|o| o.problems.iter())
            .any(|p| p.message.contains("could not be read")),
        "{failure:?}"
    );
}
