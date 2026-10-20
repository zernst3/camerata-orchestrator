//! W25 — no rule's EMITTED directive is written in scanner voice.
//!
//! A selected option's `directive` is emitted verbatim into the client's own
//! `AGENTS.md`/`CONVENTIONS.md` (`arm.rs::render_rule`, `report_export.rs:2073`) as a
//! convention the project follows. A directive that opens "Scan column declarations…" or
//! "Flag as high severity:" tells the reader — a client engineer, or the client's own AI
//! agents reading CONVENTIONS.md as governance — to run a scanner, which is Camerata's job,
//! not theirs. The same text is also the scanning model's rule list, where a regex-engine
//! instruction is equally wrong.
//!
//! The supabase pack was revoiced in `12e8f69`; this gate covers the whole corpus, so the
//! defect cannot creep back in on a new rule or survive in the 15 non-supabase rules this
//! gate was written to clear. It checks DIRECTIVES, which ship; it deliberately does not
//! check `qualifies`, which is where the scanner mechanics (pattern lists, severity keying)
//! correctly live.

use camerata_rules::{corpus_path, load_corpus_lenient};

/// Opens as an instruction to the scanner rather than a statement of what the code must be.
fn reads_as_scanner_instruction(directive: &str) -> bool {
    let d = directive.trim_start();
    const OPENERS: &[&str] = &[
        "Scan ", "Scan for", "Regex-scan", "Parse each", "Parse every", "Trace ", "Survey ",
        "Emit a finding", "Emit a ", "Flag as ", "Flag every", "Grep ",
    ];
    OPENERS.iter().any(|o| d.starts_with(o))
}

#[tokio::test]
async fn req_w25_no_emitted_directive_reads_as_a_scanner_instruction() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let mut offenders: Vec<String> = Vec::new();
    for rule in corpus.iter() {
        for opt in &rule.options {
            // Every option's directive can be emitted (the alternatives pass can select any
            // of them), so all are checked, not only the default.
            if reads_as_scanner_instruction(&opt.directive) {
                offenders.push(format!(
                    "{} / {}: {:?}",
                    rule.id.0,
                    opt.id,
                    opt.directive.chars().take(70).collect::<String>()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these directives ship into the client's CONVENTIONS.md in scanner voice; state the \
         convention the code must follow and move scan mechanics to `qualifies`:\n  {}",
        offenders.join("\n  ")
    );
}
