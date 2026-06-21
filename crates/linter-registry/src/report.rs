//! Citation-validation report generator.
//!
//! Scans all rule TOML files under a corpus directory, extracts linter
//! citations from each rule's `qualifies` field, validates them against
//! [`LinterRegistry`], and writes a 3-column Markdown report to
//! `docs/rule-grounding/citation-validation.md`.
//!
//! # Report columns
//!
//! | column | meaning |
//! |---|---|
//! | `resolves` | The cited (tool, id) pair is in the known-good list |
//! | `not-found` | The tool is known but the id was not found in its list |
//! | `unsourced` | No linter citation could be extracted from the rule |
//!
//! # Extraction heuristic
//!
//! The extractor scans the `qualifies` string for recognisable tool names
//! followed by a rule-id pattern:
//!
//! - `clippy::<ident>` — Clippy lints
//! - `Ruff [A-Z][0-9]{3}` — Ruff codes
//! - `Ruff E722 / flake8 bare-except` style references
//! - `@typescript-eslint/<ident>` — TypeScript ESLint
//! - `react-hooks/<ident>` — React Hooks plugin
//! - `ESLint no-<something>` — ESLint core rules
//! - `Bandit B[0-9]{3}` — Bandit rules
//! - `Roslyn CA[0-9]{4}` / `IDE[0-9]{4}` — Roslyn rules
//! - `errcheck` / `staticcheck` — golangci-lint linter names
//! - `RuboCop` — RuboCop (rule name not always extractable)
//! - `Checkstyle` — Checkstyle (check name not always extractable)
//! - `SpotBugs` — SpotBugs (pattern not always extractable)
//! - `sqlfluff` — SQLFluff (no specific ID in current corpus)
//!
//! When no citation is extractable, the rule is classified as `unsourced`.

use std::path::{Path, PathBuf};

use crate::registry::{CitationStatus, LinterRegistry};

/// One extracted citation from a rule file.
#[derive(Debug, Clone)]
pub struct ExtractedCitation {
    /// The linter tool key (lowercase).
    pub tool: String,
    /// The rule id as extracted from the text.
    pub rule_id: String,
}

/// One row in the report.
#[derive(Debug)]
pub struct ReportRow {
    /// Rule id from the TOML file.
    pub rule_id: String,
    /// Extracted citations (may be empty → unsourced).
    pub citations: Vec<ExtractedCitation>,
    /// Validation outcome for each citation, or None when unsourced.
    pub statuses: Vec<CitationStatus>,
}

impl ReportRow {
    /// Primary status for report bucketing.
    pub fn primary_status(&self) -> &'static str {
        if self.citations.is_empty() {
            return "unsourced";
        }
        // If any citation resolves, treat the row as resolving.
        if self.statuses.iter().any(|s| *s == CitationStatus::Resolves) {
            return "resolves";
        }
        "not-found"
    }
}

/// Scan corpus TOML files and generate the citation-validation report.
///
/// Writes to `<output_path>` (created/overwritten). Returns the report
/// markdown as a `String` as well as a list of errors encountered during
/// scanning (file I/O or TOML parse failures; skipped rows).
///
/// `corpus_dir` is typically `crates/rules/principles/`.
/// `output_path` is `docs/rule-grounding/citation-validation.md`.
pub fn generate_report(
    corpus_dir: &Path,
    output_path: &Path,
    registry: &LinterRegistry,
) -> Result<(String, Vec<String>), std::io::Error> {
    let mut errors: Vec<String> = Vec::new();

    // Collect TOML paths.
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_toml_paths_sync(corpus_dir, &mut paths, &mut errors);
    paths.sort();

    // Build rows.
    let mut rows: Vec<ReportRow> = Vec::new();
    for path in &paths {
        match build_row(path, registry) {
            Ok(row) => rows.push(row),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }

    // Render markdown.
    let md = render_markdown(&rows, corpus_dir);

    // Write to output.
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, md.as_bytes())?;

    Ok((md, errors))
}

// ─── Internal helpers ────────────────────────────────────────────────────────

fn collect_toml_paths_sync(dir: &Path, out: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            errors.push(format!("Cannot read {}: {e}", dir.display()));
            return;
        }
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_toml_paths_sync(&path, out, errors);
        } else if path.extension().map(|e| e == "toml").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn build_row(path: &Path, registry: &LinterRegistry) -> Result<ReportRow, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read error: {e}"))?;

    // Extract rule id from id = "..." line.
    let rule_id = extract_toml_field(&text, "id")
        .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().to_string());

    // Extract qualifies field for citation scanning.
    let qualifies = extract_toml_field(&text, "qualifies").unwrap_or_default();

    let citations = extract_citations(&qualifies);
    let statuses: Vec<CitationStatus> = citations
        .iter()
        .map(|c| registry.validate(&c.tool, &c.rule_id))
        .collect();

    Ok(ReportRow {
        rule_id,
        citations,
        statuses,
    })
}

/// Extract a simple string field from a TOML source (best-effort, no full parse).
fn extract_toml_field(text: &str, field: &str) -> Option<String> {
    let prefix = format!("{field} = \"");
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(prefix.as_str()) {
            let rest = &trimmed[prefix.len()..];
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_owned());
            }
        }
    }
    None
}

/// Extract linter citations from a `qualifies` string.
///
/// Heuristic-based; will miss citations with unusual formatting.
fn extract_citations(text: &str) -> Vec<ExtractedCitation> {
    let mut citations: Vec<ExtractedCitation> = Vec::new();

    // clippy::<ident>
    extract_pattern(text, r"clippy::([a-z_]+)", "clippy", &mut citations);

    // @typescript-eslint/<ident>
    extract_pattern(
        text,
        r"@typescript-eslint/([a-z][a-z0-9-]+)",
        "typescript-eslint",
        &mut citations,
    );

    // react-hooks/<ident>
    extract_pattern(
        text,
        r"react-hooks/([a-z][a-z-]+)",
        "react-hooks",
        &mut citations,
    );

    // Ruff codes: letter(s) followed by 3-4 digits, e.g. E722, BLE001, S608
    extract_ruff_codes(text, &mut citations);

    // Bandit B-codes
    extract_pattern(text, r"Bandit\s+(B[0-9]{3})", "bandit", &mut citations);
    // Bandit standalone Bxxx references (e.g. "B105 / B106")
    extract_standalone_b_codes(text, &mut citations);

    // Roslyn CA rules
    extract_pattern(text, r"(CA[0-9]{4})", "roslyn", &mut citations);

    // Roslyn IDE style rules
    extract_pattern(text, r"(IDE[0-9]{4})", "roslyn-style", &mut citations);

    // ESLint core: no-<something> (not prefixed with @typescript-eslint/)
    extract_eslint_no_rules(text, &mut citations);

    // errcheck (golangci-lint linter name)
    if text.contains("errcheck") {
        citations.push(ExtractedCitation {
            tool: "golangci-lint".to_owned(),
            rule_id: "errcheck".to_owned(),
        });
    }

    // staticcheck
    if text.contains("staticcheck") {
        citations.push(ExtractedCitation {
            tool: "golangci-lint".to_owned(),
            rule_id: "staticcheck".to_owned(),
        });
    }

    // RuboCop (tool name only, no specific cop extractable from generic mentions)
    if text.contains("RuboCop") || text.contains("Rubocop") {
        // Only emit if we didn't already extract a specific cop via a pattern.
        // Since the corpus says "Enforced by RuboCop with the FrozenStringLiteral cop"
        // we can pick that up:
        if text.contains("FrozenStringLiteral") {
            citations.push(ExtractedCitation {
                tool: "rubocop".to_owned(),
                // Store in lowercase to match the normalised registry entries.
                rule_id: "style/frozenstringliteralcomment".to_owned(),
            });
        }
    }

    // Deduplicate: same tool + rule_id pair.
    citations.dedup_by(|a, b| a.tool == b.tool && a.rule_id == b.rule_id);
    citations
}

/// Scan for Ruff-style codes: one or more uppercase letters then 3-4 digits.
/// E.g.: E722, BLE001, S608, B105 (when followed by a Ruff context).
fn extract_ruff_codes(text: &str, out: &mut Vec<ExtractedCitation>) {
    // Only emit Ruff codes if the text mentions Ruff, flake8, or pycodestyle.
    let is_ruff_context = text.contains("Ruff")
        || text.contains("ruff")
        || text.contains("flake8")
        || text.contains("pycodestyle")
        || text.contains("BLE")
        || text.contains("BLE001");

    if !is_ruff_context {
        return;
    }

    // Match codes like E722, BLE001, S608, B105, W503, etc.
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0_usize;
    while i < n {
        // Look for start of a potential code: uppercase letter.
        if chars[i].is_ascii_uppercase() {
            let start = i;
            // Consume letters.
            while i < n && chars[i].is_ascii_uppercase() {
                i += 1;
            }
            let prefix_len = i - start;
            if prefix_len == 0 {
                i += 1;
                continue;
            }
            // Consume digits (3-4).
            let digit_start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            let digit_len = i - digit_start;

            if digit_len >= 3 && digit_len <= 4 {
                // Must NOT be followed by another letter or digit (avoid matching
                // CA1031 as a Ruff code — CA rules are handled separately).
                let followed_by_alnum =
                    i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_');

                // Skip if this is a CA/IDE code (handled by Roslyn pattern).
                let code: String = chars[start..i].iter().collect();
                let is_roslyn = code.starts_with("CA") || code.starts_with("IDE");

                if !followed_by_alnum && !is_roslyn {
                    out.push(ExtractedCitation {
                        tool: "ruff".to_owned(),
                        rule_id: code.to_ascii_lowercase(),
                    });
                }
            }
        } else {
            i += 1;
        }
    }
}

/// Extract standalone Bandit B-codes (e.g. "B105 / B106 / B107").
fn extract_standalone_b_codes(text: &str, out: &mut Vec<ExtractedCitation>) {
    // Only in Bandit context.
    if !text.contains("Bandit") && !text.contains("bandit") {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0_usize;
    while i + 4 <= n {
        if chars[i] == 'B' && chars[i + 1].is_ascii_digit() && chars[i + 2].is_ascii_digit() && chars[i + 3].is_ascii_digit() {
            // Make sure not preceded by an uppercase letter (avoid "BLE001" etc.)
            let preceded_by_upper = i > 0 && chars[i - 1].is_ascii_uppercase();
            let followed_by_alnum = i + 4 < n && chars[i + 4].is_ascii_alphanumeric();
            if !preceded_by_upper && !followed_by_alnum {
                let code: String = chars[i..i + 4].iter().collect();
                out.push(ExtractedCitation {
                    tool: "bandit".to_owned(),
                    rule_id: code.to_ascii_lowercase(),
                });
            }
            i += 4;
        } else {
            i += 1;
        }
    }
}

/// Extract `no-<something>` ESLint core rules (not prefixed by @typescript-eslint/).
fn extract_eslint_no_rules(text: &str, out: &mut Vec<ExtractedCitation>) {
    if !text.contains("ESLint") && !text.contains("eslint") {
        return;
    }
    // Find "no-" occurrences not preceded by "/" (that would be a plugin rule).
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0_usize;
    while i + 3 < n {
        if bytes[i] == b'n' && bytes[i + 1] == b'o' && bytes[i + 2] == b'-' {
            // Check not preceded by "/".
            let preceded_by_slash = i > 0 && bytes[i - 1] == b'/';
            if !preceded_by_slash {
                // Collect the rule name.
                let mut j = i;
                while j < n && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
                    j += 1;
                }
                if j > i + 3 {
                    // Must end with an identifier character (not just "no-")
                    let rule = std::str::from_utf8(&bytes[i..j]).unwrap_or_default();
                    if rule.len() > 3 {
                        out.push(ExtractedCitation {
                            tool: "eslint".to_owned(),
                            rule_id: rule.to_owned(),
                        });
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
}

/// Simple pattern-based extractor using a hand-coded mini-regex substitute.
///
/// For each match of `pattern_prefix` + capture in `text`, push a citation.
/// This avoids pulling in the `regex` crate.
fn extract_pattern(
    text: &str,
    _pattern: &str,
    tool: &str,
    out: &mut Vec<ExtractedCitation>,
) {
    // Dispatch to the specific extractor by tool.
    match tool {
        "clippy" => {
            // Look for "clippy::<ident>"
            let mut start = 0_usize;
            while let Some(pos) = text[start..].find("clippy::") {
                let abs = start + pos + "clippy::".len();
                let rest = &text[abs..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                if end > 0 {
                    out.push(ExtractedCitation {
                        tool: tool.to_owned(),
                        rule_id: rest[..end].to_owned(),
                    });
                }
                start = abs + end;
                if start >= text.len() {
                    break;
                }
            }
        }
        "typescript-eslint" => {
            let prefix = "@typescript-eslint/";
            let mut start = 0_usize;
            while let Some(pos) = text[start..].find(prefix) {
                let abs = start + pos + prefix.len();
                let rest = &text[abs..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                    .unwrap_or(rest.len());
                if end > 0 {
                    out.push(ExtractedCitation {
                        tool: tool.to_owned(),
                        rule_id: rest[..end].to_owned(),
                    });
                }
                start = abs + end;
                if start >= text.len() {
                    break;
                }
            }
        }
        "react-hooks" => {
            let prefix = "react-hooks/";
            let mut start = 0_usize;
            while let Some(pos) = text[start..].find(prefix) {
                let abs = start + pos + prefix.len();
                let rest = &text[abs..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                    .unwrap_or(rest.len());
                if end > 0 {
                    out.push(ExtractedCitation {
                        tool: tool.to_owned(),
                        rule_id: rest[..end].to_owned(),
                    });
                }
                start = abs + end;
                if start >= text.len() {
                    break;
                }
            }
        }
        "roslyn" => {
            // CA followed by 4 digits
            let bytes = text.as_bytes();
            let n = bytes.len();
            let mut i = 0_usize;
            while i + 6 <= n {
                if bytes[i] == b'C' && bytes[i + 1] == b'A'
                    && bytes[i + 2].is_ascii_digit()
                    && bytes[i + 3].is_ascii_digit()
                    && bytes[i + 4].is_ascii_digit()
                    && bytes[i + 5].is_ascii_digit()
                {
                    let code = std::str::from_utf8(&bytes[i..i + 6]).unwrap_or_default();
                    out.push(ExtractedCitation {
                        tool: tool.to_owned(),
                        rule_id: code.to_ascii_lowercase(),
                    });
                    i += 6;
                } else {
                    i += 1;
                }
            }
        }
        "roslyn-style" => {
            // IDE followed by 4 digits
            let bytes = text.as_bytes();
            let n = bytes.len();
            let mut i = 0_usize;
            while i + 7 <= n {
                if bytes[i] == b'I' && bytes[i + 1] == b'D' && bytes[i + 2] == b'E'
                    && bytes[i + 3].is_ascii_digit()
                    && bytes[i + 4].is_ascii_digit()
                    && bytes[i + 5].is_ascii_digit()
                    && bytes[i + 6].is_ascii_digit()
                {
                    let code = std::str::from_utf8(&bytes[i..i + 7]).unwrap_or_default();
                    out.push(ExtractedCitation {
                        tool: tool.to_owned(),
                        rule_id: code.to_ascii_lowercase(),
                    });
                    i += 7;
                } else {
                    i += 1;
                }
            }
        }
        "bandit" => {
            // "Bandit B608" style
            let prefix = "Bandit ";
            let mut start = 0_usize;
            while let Some(pos) = text[start..].find(prefix) {
                let abs = start + pos + prefix.len();
                let rest = &text[abs..];
                // Expect B followed by digits.
                if rest.starts_with('B') {
                    let end = rest
                        .find(|c: char| !c.is_ascii_alphanumeric())
                        .unwrap_or(rest.len());
                    if end >= 4 {
                        out.push(ExtractedCitation {
                            tool: tool.to_owned(),
                            rule_id: rest[..end].to_ascii_lowercase(),
                        });
                    }
                }
                start = abs + 1;
                if start >= text.len() {
                    break;
                }
            }
        }
        _ => {}
    }
}

/// Render the report as Markdown.
fn render_markdown(rows: &[ReportRow], corpus_dir: &Path) -> String {
    let total = rows.len();
    let resolves_count = rows.iter().filter(|r| r.primary_status() == "resolves").count();
    let not_found_count = rows.iter().filter(|r| r.primary_status() == "not-found").count();
    let unsourced_count = rows.iter().filter(|r| r.primary_status() == "unsourced").count();

    let mut md = String::new();
    md.push_str("# Citation Validation Report\n\n");
    md.push_str(&format!(
        "Generated by `camerata-linter-registry`. Corpus: `{}`\n\n",
        corpus_dir.display()
    ));
    md.push_str("## Summary\n\n");
    md.push_str(&format!(
        "| Outcome | Count |\n|---|---|\n| resolves | {resolves_count} |\n| not-found | {not_found_count} |\n| unsourced | {unsourced_count} |\n| **total** | **{total}** |\n\n"
    ));

    md.push_str("## Detail\n\n");
    md.push_str("| Rule ID | Outcome | Citations |\n|---|---|---|\n");

    let mut sorted_rows: Vec<&ReportRow> = rows.iter().collect();
    sorted_rows.sort_by_key(|r| r.rule_id.as_str());

    for row in sorted_rows {
        let status = row.primary_status();
        let citations_text = if row.citations.is_empty() {
            "_none_".to_owned()
        } else {
            row.citations
                .iter()
                .zip(row.statuses.iter())
                .map(|(c, s)| format!("`{}::{}` → {}", c.tool, c.rule_id, s))
                .collect::<Vec<_>>()
                .join(", ")
        };
        md.push_str(&format!("| `{}` | {} | {} |\n", row.rule_id, status, citations_text));
    }

    md.push_str("\n---\n\n*Run `cargo run -p camerata-linter-registry --example generate-report` to regenerate.*\n");
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_citations_clippy_unwrap_used() {
        let text = r#"Enforced by clippy::unwrap_used and clippy::expect_used"#;
        let cits = extract_citations(text);
        let ids: Vec<&str> = cits.iter().map(|c| c.rule_id.as_str()).collect();
        assert!(ids.contains(&"unwrap_used"), "should extract unwrap_used");
        assert!(ids.contains(&"expect_used"), "should extract expect_used");
        assert!(cits.iter().all(|c| c.tool == "clippy"));
    }

    #[test]
    fn extract_citations_ruff_e722_and_ble001() {
        let text = r#"Enforced by Ruff E722 / flake8 bare-except, plus BLE001"#;
        let cits = extract_citations(text);
        let ids: Vec<&str> = cits.iter().map(|c| c.rule_id.as_str()).collect();
        assert!(ids.contains(&"e722"), "should extract E722");
        assert!(ids.contains(&"ble001"), "should extract BLE001");
    }

    #[test]
    fn extract_citations_bandit_b608() {
        let text = r#"Bandit B608 / Ruff S608 / Semgrep"#;
        let cits = extract_citations(text);
        let bandit: Vec<&str> = cits
            .iter()
            .filter(|c| c.tool == "bandit")
            .map(|c| c.rule_id.as_str())
            .collect();
        assert!(bandit.contains(&"b608"), "should extract B608");
    }

    #[test]
    fn extract_citations_typescript_eslint() {
        let text = r#"@typescript-eslint/no-explicit-any and @typescript-eslint/no-floating-promises"#;
        let cits = extract_citations(text);
        let ids: Vec<&str> = cits.iter().map(|c| c.rule_id.as_str()).collect();
        assert!(ids.contains(&"no-explicit-any"));
        assert!(ids.contains(&"no-floating-promises"));
    }

    #[test]
    fn extract_citations_react_hooks() {
        let text = r#"react-hooks/exhaustive-deps"#;
        let cits = extract_citations(text);
        assert!(
            cits.iter().any(|c| c.tool == "react-hooks" && c.rule_id == "exhaustive-deps"),
            "should extract exhaustive-deps"
        );
    }

    #[test]
    fn extract_citations_roslyn_ca() {
        let text = r#"Roslyn analyzers (CA1031, CA1068)"#;
        let cits = extract_citations(text);
        let roslyn: Vec<&str> = cits
            .iter()
            .filter(|c| c.tool == "roslyn")
            .map(|c| c.rule_id.as_str())
            .collect();
        assert!(roslyn.contains(&"ca1031"), "should extract CA1031");
        assert!(roslyn.contains(&"ca1068"), "should extract CA1068");
    }

    #[test]
    fn extract_citations_errcheck() {
        let text = r#"static-analysis gate (errcheck, golangci-lint errcheck)"#;
        let cits = extract_citations(text);
        assert!(
            cits.iter()
                .any(|c| c.tool == "golangci-lint" && c.rule_id == "errcheck"),
            "should extract errcheck"
        );
    }

    #[test]
    fn extract_citations_rubocop_frozen_string_literal() {
        let text = r#"Enforced by RuboCop with the FrozenStringLiteral cop enabled"#;
        let cits = extract_citations(text);
        assert!(
            cits.iter().any(|c| c.tool == "rubocop"),
            "should extract at least one rubocop citation"
        );
    }

    #[test]
    fn extract_citations_no_linter_returns_empty() {
        let text = r#"Prose check, enforced by code review only"#;
        let cits = extract_citations(text);
        assert!(cits.is_empty(), "no linter citations → empty");
    }

    #[test]
    fn report_row_primary_status_unsourced_when_no_citations() {
        let row = ReportRow {
            rule_id: "RULE-X".to_owned(),
            citations: vec![],
            statuses: vec![],
        };
        assert_eq!(row.primary_status(), "unsourced");
    }

    #[test]
    fn report_row_primary_status_resolves_when_any_resolves() {
        let row = ReportRow {
            rule_id: "RULE-X".to_owned(),
            citations: vec![ExtractedCitation {
                tool: "clippy".to_owned(),
                rule_id: "unwrap_used".to_owned(),
            }],
            statuses: vec![CitationStatus::Resolves],
        };
        assert_eq!(row.primary_status(), "resolves");
    }

    #[test]
    fn report_row_primary_status_not_found_when_tool_known_but_id_missing() {
        let row = ReportRow {
            rule_id: "RULE-X".to_owned(),
            citations: vec![ExtractedCitation {
                tool: "clippy".to_owned(),
                rule_id: "made_up_lint".to_owned(),
            }],
            statuses: vec![CitationStatus::NotFound],
        };
        assert_eq!(row.primary_status(), "not-found");
    }

    #[test]
    fn render_markdown_produces_valid_structure() {
        let rows = vec![
            ReportRow {
                rule_id: "RUST-NO-UNWRAP-1".to_owned(),
                citations: vec![ExtractedCitation {
                    tool: "clippy".to_owned(),
                    rule_id: "unwrap_used".to_owned(),
                }],
                statuses: vec![CitationStatus::Resolves],
            },
            ReportRow {
                rule_id: "PROSE-ONLY-1".to_owned(),
                citations: vec![],
                statuses: vec![],
            },
        ];
        let corpus_dir = Path::new("crates/rules/principles");
        let md = render_markdown(&rows, corpus_dir);

        assert!(md.contains("# Citation Validation Report"), "has title");
        assert!(md.contains("| resolves |"), "has resolves row");
        assert!(md.contains("| not-found |"), "has not-found row");
        assert!(md.contains("| unsourced |"), "has unsourced row");
        assert!(md.contains("RUST-NO-UNWRAP-1"), "contains rule id");
        assert!(md.contains("PROSE-ONLY-1"), "contains prose rule id");
    }
}
