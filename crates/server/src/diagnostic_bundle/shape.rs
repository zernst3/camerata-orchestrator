//! The shape probe: how the files of a failed part are built, as numbers and yes-or-no values
//! only (design section 4). A parse failure is often a matter of line endings, a byte-order
//! mark, a huge line or bytes that are not UTF-8, which this shows without one byte of text.
//!
//! [`probe`] reads each file and keeps only the [`FileShape`] fields. No text leaves the
//! function: the shape has no path (the caller attaches the path token) and no content field.

use std::io::Read;
use std::path::{Component, Path};

use serde::Serialize;

/// The most files one bundle probes.
pub const MAX_PROBED_FILES: usize = 500;

/// How a file ends its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    Lf,
    Crlf,
    /// Both `\n` and `\r\n` line ends.
    Mixed,
    /// No line end at all.
    None,
}

/// The shape of one file. Every field but `present` is `None` when the file could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileShape {
    /// Whether the file was there and readable inside the checkout.
    pub present: bool,
    pub bytes: Option<u64>,
    /// Line count: line ends, plus one for a last line with no line end.
    pub lines: Option<u64>,
    /// The longest line in bytes, its line end not counted.
    pub longest_line: Option<u64>,
    pub line_ending: Option<LineEnding>,
    pub utf8: Option<bool>,
    pub bom: Option<bool>,
    pub ends_with_newline: Option<bool>,
}

/// The shape of each of `files` (repo-relative) under `repo_dir`, in order, at most
/// [`MAX_PROBED_FILES`]. A path that is absolute, climbs out with `..`, or resolves outside the
/// checkout (a symbolic link) is not read and shows as not present.
pub fn probe(repo_dir: &Path, files: &[String]) -> Vec<FileShape> {
    let root = repo_dir.canonicalize().ok();
    files
        .iter()
        .take(MAX_PROBED_FILES)
        .map(|f| match &root {
            Some(root) => shape_of(root, f).unwrap_or_default(),
            None => FileShape::default(),
        })
        .collect()
}

fn shape_of(root: &Path, file: &str) -> Option<FileShape> {
    let rel = Path::new(file);
    if !rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let path = root.join(rel).canonicalize().ok()?;
    if !path.starts_with(root) || !path.is_file() {
        return None;
    }
    let mut reader = std::fs::File::open(&path).ok()?;
    let mut m = Measure::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        m.feed(&buf[..n]);
    }
    Some(m.finish())
}

/// The running measure of one file, fed in chunks.
#[derive(Default)]
struct Measure {
    bytes: u64,
    first: Vec<u8>,
    lf: u64,
    crlf: u64,
    current_line: u64,
    longest: u64,
    last: Option<u8>,
    prev: Option<u8>,
    /// Bytes of a UTF-8 sequence cut by the chunk boundary.
    carry: Vec<u8>,
    utf8: bool,
    started: bool,
}

impl Measure {
    fn feed(&mut self, chunk: &[u8]) {
        if !self.started {
            self.started = true;
            self.utf8 = true;
        }
        if self.first.len() < 3 {
            let need = 3 - self.first.len();
            self.first.extend(chunk.iter().take(need));
        }
        for &b in chunk {
            self.bytes += 1;
            if b == b'\n' {
                if self.prev == Some(b'\r') {
                    self.crlf += 1;
                    // The `\r` was counted into the line; it belongs to the line end.
                    self.current_line = self.current_line.saturating_sub(1);
                } else {
                    self.lf += 1;
                }
                self.longest = self.longest.max(self.current_line);
                self.current_line = 0;
            } else {
                self.current_line += 1;
            }
            self.prev = Some(b);
            self.last = Some(b);
        }
        if self.utf8 {
            let mut joined = std::mem::take(&mut self.carry);
            joined.extend_from_slice(chunk);
            match std::str::from_utf8(&joined) {
                Ok(_) => {}
                Err(e) if e.error_len().is_none() => {
                    self.carry = joined[e.valid_up_to()..].to_vec();
                }
                Err(_) => self.utf8 = false,
            }
        }
    }

    fn finish(mut self) -> FileShape {
        if !self.carry.is_empty() {
            self.utf8 = false;
        }
        if !self.started {
            self.utf8 = true;
        }
        self.longest = self.longest.max(self.current_line);
        let ends = self.last == Some(b'\n');
        let line_ends = self.lf + self.crlf;
        let lines = line_ends + u64::from(self.bytes > 0 && !ends);
        let line_ending = match (self.lf, self.crlf) {
            (0, 0) => LineEnding::None,
            (_, 0) => LineEnding::Lf,
            (0, _) => LineEnding::Crlf,
            _ => LineEnding::Mixed,
        };
        FileShape {
            present: true,
            bytes: Some(self.bytes),
            lines: Some(lines),
            longest_line: Some(self.longest),
            line_ending: Some(line_ending),
            utf8: Some(self.utf8),
            bom: Some(self.first.starts_with(&[0xEF, 0xBB, 0xBF])),
            ends_with_newline: Some(ends),
        }
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    fn shape(bytes: &[u8]) -> FileShape {
        let mut m = Measure::default();
        // Feed one byte at a time, so every chunk boundary is exercised.
        for b in bytes {
            m.feed(std::slice::from_ref(b));
        }
        m.finish()
    }

    #[test]
    fn line_endings_bom_and_utf8() {
        assert_eq!(shape(b"a\nbb\n").line_ending, Some(LineEnding::Lf));
        assert_eq!(shape(b"a\r\nb\n").line_ending, Some(LineEnding::Mixed));
        assert_eq!(shape(b"abc").line_ending, Some(LineEnding::None));
        assert_eq!(shape(b"abc").lines, Some(1));
        assert_eq!(shape(b"").lines, Some(0));
        assert_eq!(shape(b"a\r\nbbb\r\n").longest_line, Some(3));
        assert_eq!(shape("\u{feff}é\n".as_bytes()).bom, Some(true));
        assert_eq!(shape("é€\n".as_bytes()).utf8, Some(true));
        assert_eq!(shape(b"\xff\xfe").utf8, Some(false));
        assert_eq!(shape(b"\xe2\x82").utf8, Some(false));
    }

    #[test]
    fn a_path_outside_the_checkout_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(dir.path().join("outside.txt"), "x\n").unwrap();
        let shapes = probe(
            &repo,
            &[
                "../outside.txt".to_string(),
                dir.path()
                    .join("outside.txt")
                    .to_string_lossy()
                    .into_owned(),
            ],
        );
        assert!(shapes.iter().all(|s| !s.present));
    }
}
