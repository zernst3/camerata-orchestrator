//! This crate holds no secret-shaped literal (push protection and secret scanners match by shape).

use std::path::{Path, PathBuf};

use crate::{detect_secrets, SecretShape};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_secret_shaped_literal_in_this_crates_source() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        files.len() >= 8,
        "the scan must actually see the crate's sources"
    );
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let hits = detect_secrets(&text);
        // Report the file, shape and line only, never the text.
        let located: Vec<(SecretShape, usize)> = hits
            .iter()
            .map(|m| (m.shape, text[..m.start].matches('\n').count() + 1))
            .collect();
        assert!(
            located.is_empty(),
            "{} holds secret-shaped literal(s) at (shape, line): {located:?}",
            file.display(),
        );
    }
}
