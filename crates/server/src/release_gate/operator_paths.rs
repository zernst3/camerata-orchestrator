//! The gate's operator-paths check: no client file names a folder of the operator's own machine.
//!
//! A tool's words, a panic message or a copied error can carry the path of the machine that ran
//! the inspection. The folders that identify that machine are the home directory at export time,
//! the folders the engine keeps its state and scan copies in, each repo's clone or scan folder,
//! and the temp folder. A client file that names one of them is refused, and the refusal names
//! the file and the path, to the operator only.
//!
//! The check matches those folders, never `/home/` or the macOS home root as such: a client's own code
//! quotes paths like `/home/node` and `/home/app`. A folder with fewer than two parts (`/root`,
//! `/home`) and the shared temp folders (`/tmp`, `/var/tmp`) are not matched either, because
//! client code quotes those the same way. A folder is not matched when more name follows it, so
//! `/home/ann` does not match `/home/anna`; what stands before it does not matter.
//!
//! It reads the same surfaces as the operator-only check: every member's name and text, every part
//! and string of the workbook, and the PDF's text and data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::archive::{read_zip_entries, xlsx_cells, xlsx_strings};
use super::report::Problem;
use super::text_style::PdfText;

/// Shared temp folders a client's code quotes; never the operator's own.
const SHARED_FOLDERS: [&str; 4] = ["/tmp", "/var/tmp", "/private/tmp", "/private/var/tmp"];

/// The operator's own folders, as the strings to look for, from the environment and `extra` (the
/// clone and scan folders of the project's repos). Each folder is listed as given and, when it
/// resolves, as resolved (`/var/...` is `/private/var/...` on macOS).
pub fn roots(extra: &[PathBuf]) -> Vec<String> {
    let storage = crate::client_source::storage::StorageRoots::from_environment();
    let mut folders: Vec<PathBuf> = Vec::new();
    folders.extend(storage.home);
    folders.extend(storage.data_dir);
    folders.extend(storage.cli_state_dir);
    folders.push(crate::scan_mirror::mirrors_root());
    folders.push(std::env::temp_dir());
    folders.extend(extra.iter().cloned());
    let mut out: Vec<String> = Vec::new();
    for folder in folders {
        for candidate in [Some(folder.clone()), folder.canonicalize().ok()]
            .into_iter()
            .flatten()
        {
            let text = candidate
                .to_string_lossy()
                .trim_end_matches('/')
                .to_string();
            if is_identifying(&text) && !out.contains(&text) {
                out.push(text);
            }
        }
    }
    out
}

/// The problem to report when the home folder is not known, so the check could not look for it:
/// none when `home` is known. Fail closed: a package is not released on a check that did not run.
///
/// A home folder with fewer than two parts (`/root`) is known but is not a root to look for, so it
/// is not checked and is not a problem.
pub fn home_problems(home: Option<&Path>) -> Vec<Problem> {
    match home {
        Some(_) => Vec::new(),
        None => vec![Problem::new(
            "(package)",
            "operator home folder",
            "the operator's home folder could not be read, so the check for it could not run",
            None,
        )],
    }
}

/// Whether `folder` names the operator's machine rather than a place every machine has.
fn is_identifying(folder: &str) -> bool {
    let parts = Path::new(folder)
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    Path::new(folder).is_absolute() && parts >= 2 && !SHARED_FOLDERS.contains(&folder)
}

/// Whether `c` can continue a name, so a match that touches it on the right is part of a longer
/// name (`/home/ann` is not `/home/anna`).
fn continues_a_name(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Whether `text` names `root`: not followed by more of a name. What stands before the root does
/// not matter: a root of two or more parts that appears anywhere in a path is a leak, and what
/// sticks to its left is often an escape (`\n` in JSON, a colour code) or a longer path
/// (`/System/Volumes/Data/home/ann`).
fn names_path(text: &str, root: &str) -> bool {
    text.match_indices(root).any(|(at, found)| {
        let mut rest = text[at + found.len()..].chars();
        let after = match (rest.next(), rest.next()) {
            // A full stop that ends a sentence is not part of the name.
            (Some('.'), next) => next.filter(|c| c.is_alphanumeric()).map(|_| '.'),
            (first, _) => first,
        };
        !after.is_some_and(continues_a_name)
    })
}

/// One problem per root named in `text`.
pub fn text_problems(file: &str, location: &str, text: &str, roots: &[String]) -> Vec<Problem> {
    roots
        .iter()
        .filter(|root| names_path(text, root))
        .map(|root| {
            Problem::new(
                file,
                location,
                "a folder of the operator's own machine is named in a client file",
                Some(root.clone()),
            )
        })
        .collect()
}

/// A workbook: every part as text, then its cells and strings.
fn xlsx_problems(file: &str, bytes: &[u8], roots: &[String]) -> Vec<Problem> {
    let mut problems = Vec::new();
    if let Ok(parts) = read_zip_entries(bytes) {
        for (part, part_bytes) in &parts {
            problems.extend(text_problems(
                file,
                &format!("workbook part {part}"),
                &String::from_utf8_lossy(part_bytes),
                roots,
            ));
        }
    }
    let mut texts: Vec<(String, String)> = Vec::new();
    if let Ok(cells) = xlsx_cells(bytes) {
        texts.extend(
            cells
                .into_iter()
                .map(|c| (format!("sheet {:?} cell {}", c.sheet, c.reference), c.text)),
        );
    }
    if let Ok(strings) = xlsx_strings(bytes) {
        texts.extend(
            strings
                .into_iter()
                .enumerate()
                .map(|(i, text)| (format!("shared string #{i}"), text)),
        );
    }
    let mut seen: std::collections::BTreeSet<String> =
        problems.iter().filter_map(|p| p.evidence.clone()).collect();
    for (location, text) in texts {
        for problem in text_problems(file, &location, &text, roots) {
            if problem
                .evidence
                .as_ref()
                .is_some_and(|root| seen.insert(root.clone()))
            {
                problems.push(problem);
            }
        }
    }
    problems
}

/// Every member of the archive, by kind. `pdf_text` is the caller's own extraction of the PDF's
/// text; `pdf_pages` is the archive pass's reading of each PDF member. A PDF whose text could not
/// be read is left to the secrets check, which refuses it.
pub fn archive_problems(
    entries: &BTreeMap<String, Vec<u8>>,
    pdf_text: Option<&str>,
    pdf_pages: &BTreeMap<String, PdfText>,
    roots: &[String],
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (name, bytes) in entries {
        problems.extend(text_problems(
            "(archive member name)",
            "file name",
            name,
            roots,
        ));
        if name.ends_with(".xlsx") {
            problems.extend(xlsx_problems(name, bytes, roots));
        } else if name.ends_with(".pdf") {
            match (pdf_text, pdf_pages.get(name)) {
                (Some(text), _) => problems.extend(text_problems(name, "PDF text", text, roots)),
                (None, Some(PdfText::Pages(pages))) => {
                    for (i, page) in pages.iter().enumerate() {
                        problems.extend(text_problems(
                            name,
                            &format!("page {}", i + 1),
                            page,
                            roots,
                        ));
                    }
                }
                (None, Some(PdfText::Unreadable(_))) => {}
                (None, _) => problems.extend(text_problems(
                    name,
                    "PDF bytes",
                    &String::from_utf8_lossy(bytes),
                    roots,
                )),
            }
        } else {
            problems.extend(text_problems(
                name,
                "file text",
                &String::from_utf8_lossy(bytes),
                roots,
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots_of(folders: &[&str]) -> Vec<String> {
        folders.iter().map(|f| f.to_string()).collect()
    }

    fn named(text: &str, root: &str) -> bool {
        !text_problems("f", "l", text, &roots_of(&[root])).is_empty()
    }

    #[test]
    fn a_folder_named_whole_is_found_and_the_problem_names_the_file_and_the_folder() {
        let problems = text_problems(
            "csv/x.csv",
            "line 2",
            "read /home/ann/work/a.rs",
            &roots_of(&["/home/ann"]),
        );
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].file, "csv/x.csv");
        assert_eq!(problems[0].evidence.as_deref(), Some("/home/ann"));
    }

    #[test]
    fn a_folder_inside_a_longer_name_is_not_found_on_the_right() {
        assert!(!named("/home/anna/x", "/home/ann"));
        assert!(!named("/home/ann-b/x", "/home/ann"));
        assert!(!named("/home/ann_b/x", "/home/ann"));
        assert!(!named("/home/ann.b/x", "/home/ann"));
        assert!(!named("/home/ann1", "/home/ann"));
    }

    #[test]
    fn a_folder_stuck_to_a_letter_or_a_code_before_it_is_still_found() {
        // A JSON-escaped newline puts an `n` before the path; a terminal colour code and its
        // escaped form put a code there; a longer path contains the root.
        for text in [
            "error:\\n/Users/auditor/x",
            "\u{1b}[31m/Users/auditor/x",
            "\\u001b[31m/Users/auditor/x",
            "/System/Volumes/Data/Users/auditor/x",
            "a/Users/auditor/x",
            "x-/Users/auditor/x",
        ] {
            assert!(named(text, "/Users/auditor"), "{text}");
        }
        // The right-hand guard stays: a longer name after the root is another folder.
        assert!(!named("/home/auditorx", "/home/auditor"));
        assert!(!named("/home/auditorx/y", "/home/auditor"));
    }

    #[test]
    fn an_unknown_home_folder_is_a_problem_and_a_known_one_is_not() {
        let problems = home_problems(None);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0]
                .message
                .contains("home folder could not be read"),
            "{}",
            problems[0].message
        );
        assert!(problems[0].evidence.is_none());
        assert!(home_problems(Some(Path::new("/home/ann"))).is_empty());
        // A one-part home (`/root`) is not checked for as a root, and is not a problem either.
        assert!(home_problems(Some(Path::new("/root"))).is_empty());
    }

    #[test]
    fn a_folder_at_the_end_of_a_sentence_or_a_quote_is_found() {
        assert!(named("saved in /home/ann.", "/home/ann"));
        assert!(named("saved in /home/ann", "/home/ann"));
        assert!(named("\"/home/ann\"", "/home/ann"));
        assert!(named("(/home/ann/x)", "/home/ann"));
        assert!(named("a /home/ann. Next", "/home/ann"));
        assert!(named("file:///home/ann/x", "/home/ann"));
    }

    #[test]
    fn a_later_match_counts_when_the_first_is_inside_a_longer_name() {
        assert!(named("/home/anna and /home/ann", "/home/ann"));
    }

    #[test]
    fn a_home_style_path_that_is_not_the_operators_is_not_found() {
        let roots = roots_of(&["/home/ann"]);
        for text in [
            "WORKDIR /home/node/app",
            "/home/app",
            "/root/.ssh",
            "/home/deploy/x",
        ] {
            assert!(text_problems("f", "l", text, &roots).is_empty(), "{text}");
        }
    }

    #[test]
    fn only_a_folder_that_identifies_a_machine_is_a_root() {
        assert!(is_identifying("/home/ann"));
        assert!(is_identifying("/var/folders/ab/cd/T"));
        assert!(!is_identifying("/root"));
        assert!(!is_identifying("/home"));
        assert!(!is_identifying("/"));
        assert!(!is_identifying(""));
        assert!(!is_identifying("relative/dir"));
        for shared in SHARED_FOLDERS {
            assert!(!is_identifying(shared), "{shared}");
        }
        assert!(is_identifying("/tmp/camerata-test/scan-mirrors"));
    }

    #[test]
    fn the_roots_hold_each_extra_folder_once_without_a_trailing_slash() {
        let extra = [
            PathBuf::from("/srv/clones/acme/portal/"),
            PathBuf::from("/srv/clones/acme/portal"),
            PathBuf::from("/tmp"),
            PathBuf::from("/root"),
        ];
        let found = roots(&extra);
        assert_eq!(
            found
                .iter()
                .filter(|r| *r == "/srv/clones/acme/portal")
                .count(),
            1
        );
        assert!(!found.iter().any(|r| r == "/tmp" || r == "/root"));
        assert!(found.iter().all(|r| !r.ends_with('/')));
    }

    #[test]
    fn the_roots_hold_the_home_directory_and_the_mirror_folder() {
        let found = roots(&[]);
        if let Some(home) = dirs::home_dir() {
            let home = home.to_string_lossy().trim_end_matches('/').to_string();
            if is_identifying(&home) {
                assert!(found.contains(&home), "{found:?}");
            }
        }
        let mirrors = crate::scan_mirror::mirrors_root();
        assert!(found.contains(&mirrors.to_string_lossy().into_owned()));
    }

    #[test]
    fn a_member_name_and_a_pdf_page_and_a_pdf_stand_in_are_all_read() {
        let roots = roots_of(&["/home/ann"]);
        // The member NAME names the folder as a whole name.
        let by_name = archive_problems(
            &BTreeMap::from([("/home/ann/r.txt".to_string(), Vec::new())]),
            None,
            &BTreeMap::new(),
            &roots,
        );
        assert!(by_name.iter().any(|p| p.file == "(archive member name)"));
        let by_text = archive_problems(
            &BTreeMap::from([("a.txt".to_string(), b"in /home/ann/x".to_vec())]),
            None,
            &BTreeMap::new(),
            &roots,
        );
        assert!(by_text.iter().any(|p| p.file == "a.txt"));
        let pages = BTreeMap::from([(
            "r.pdf".to_string(),
            PdfText::Pages(vec!["clean".into(), "in /home/ann/x".into()]),
        )]);
        let by_page = archive_problems(
            &BTreeMap::from([("r.pdf".to_string(), b"%PDF".to_vec())]),
            None,
            &pages,
            &roots,
        );
        assert!(
            by_page.iter().any(|p| p.location == "page 2"),
            "{by_page:?}"
        );
        let by_extraction = archive_problems(
            &BTreeMap::from([("r.pdf".to_string(), b"%PDF".to_vec())]),
            Some("in /home/ann/x"),
            &BTreeMap::new(),
            &roots,
        );
        assert!(by_extraction.iter().any(|p| p.location == "PDF text"));
        let stand_in = archive_problems(
            &BTreeMap::from([("r.pdf".to_string(), b"%PDF stand-in /home/ann/x".to_vec())]),
            None,
            &BTreeMap::new(),
            &roots,
        );
        assert!(stand_in.iter().any(|p| p.location == "PDF bytes"));
        let unreadable = archive_problems(
            &BTreeMap::from([("r.pdf".to_string(), b"/home/ann".to_vec())]),
            None,
            &BTreeMap::from([("r.pdf".to_string(), PdfText::Unreadable("x".into()))]),
            &roots,
        );
        assert!(unreadable.is_empty(), "{unreadable:?}");
    }
}
