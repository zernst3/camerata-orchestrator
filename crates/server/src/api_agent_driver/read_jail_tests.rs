//! The read tools (`Read`, `Glob`, `Grep`, `LS`) are confined to the worktree plus the read
//! folders the run declared. A path outside (absolute, through `..`, or through a symlink that
//! leaves the jail) is refused with a tool error the model sees, and nothing outside is read.
//!
//! Every test builds its own layout in a temp dir: `<tmp>/wt` is the worktree, `<tmp>/outside`
//! holds a file whose content must never reach a tool result.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{read_jail, ReadJail, ToolInvocation};

// The tools as the tests call them: a jail rooted at `wt` (no read folders), the model-facing
// text out.
fn jail(wt: Option<&Path>) -> ReadJail {
    ReadJail::new(wt, &[])
}
fn execute_read(inv: &ToolInvocation, wt: Option<&Path>) -> String {
    read_jail::execute_read(inv, &jail(wt)).text
}
fn execute_glob(inv: &ToolInvocation, wt: Option<&Path>) -> String {
    read_jail::execute_glob(inv, &jail(wt)).text
}
fn execute_grep(inv: &ToolInvocation, wt: Option<&Path>) -> String {
    read_jail::execute_grep(inv, &jail(wt)).text
}
fn execute_ls(inv: &ToolInvocation, wt: Option<&Path>) -> String {
    read_jail::execute_ls(inv, &jail(wt)).text
}

/// The content of the file outside the jail. A tool result containing it is a leak.
const OUTSIDE_SECRET: &str = "OUTSIDE-THE-JAIL-CONTENT";

struct Layout {
    _tmp: tempfile::TempDir,
    wt: PathBuf,
    outside_dir: PathBuf,
    outside_file: PathBuf,
}

fn layout() -> Layout {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wt = tmp.path().join("wt");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(wt.join("src/lib.rs"), "pub fn inside() {}\n").unwrap();
    let outside_file = outside_dir.join("secret.txt");
    std::fs::write(&outside_file, format!("{OUTSIDE_SECRET}\n")).unwrap();
    Layout {
        _tmp: tmp,
        wt,
        outside_dir,
        outside_file,
    }
}

fn inv(name: &str, input: serde_json::Value) -> ToolInvocation {
    ToolInvocation {
        id: "call-jail".to_string(),
        name: name.to_string(),
        input,
    }
}

fn assert_refused(what: &str, out: &str) {
    assert!(
        out.starts_with("DENIED"),
        "{what}: must be refused with a DENIED tool error, got: {out}"
    );
    assert!(
        !out.contains(OUTSIDE_SECRET),
        "{what}: content from outside the jail reached the model: {out}"
    );
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink");
}

#[test]
fn read_of_an_absolute_path_outside_the_worktree_is_refused() {
    let l = layout();
    let out = execute_read(
        &inv(
            "Read",
            json!({ "file_path": l.outside_file.display().to_string() }),
        ),
        Some(&l.wt),
    );
    assert_refused("absolute Read", &out);
}

#[test]
fn read_through_a_dotdot_escape_is_refused() {
    let l = layout();
    let out = execute_read(
        &inv("Read", json!({ "file_path": "../outside/secret.txt" })),
        Some(&l.wt),
    );
    assert_refused("`..` Read", &out);
}

#[cfg(unix)]
#[test]
fn read_through_a_symlink_pointing_out_of_the_worktree_is_refused() {
    let l = layout();
    symlink(&l.outside_file, &l.wt.join("src/innocent.txt"));
    let out = execute_read(
        &inv("Read", json!({ "file_path": "src/innocent.txt" })),
        Some(&l.wt),
    );
    assert_refused("symlinked-file Read", &out);

    symlink(&l.outside_dir, &l.wt.join("vendor"));
    let out = execute_read(
        &inv("Read", json!({ "file_path": "vendor/secret.txt" })),
        Some(&l.wt),
    );
    assert_refused("symlinked-dir Read", &out);
}

#[test]
fn glob_with_an_absolute_pattern_outside_the_worktree_is_refused() {
    let l = layout();
    let pattern = format!("{}/*", l.outside_dir.display());
    let out = execute_glob(&inv("Glob", json!({ "pattern": pattern })), Some(&l.wt));
    assert_refused("absolute Glob", &out);
    assert!(
        !out.contains("secret.txt"),
        "no outside path is listed: {out}"
    );
}

#[test]
fn glob_with_a_dotdot_pattern_is_refused() {
    let l = layout();
    let out = execute_glob(
        &inv("Glob", json!({ "pattern": "../outside/*" })),
        Some(&l.wt),
    );
    assert_refused("`..` Glob", &out);
    assert!(
        !out.contains("secret.txt"),
        "no outside path is listed: {out}"
    );
}

#[cfg(unix)]
#[test]
fn glob_never_lists_files_reached_through_a_symlink_that_leaves_the_worktree() {
    let l = layout();
    symlink(&l.outside_dir, &l.wt.join("vendor"));
    let out = execute_glob(&inv("Glob", json!({ "pattern": "**/*.txt" })), Some(&l.wt));
    assert!(
        !out.contains("secret.txt"),
        "a file outside the jail was listed through a symlink: {out}"
    );
}

#[test]
fn grep_of_a_directory_outside_the_worktree_is_refused() {
    let l = layout();
    let out = execute_grep(
        &inv(
            "Grep",
            json!({ "pattern": "OUTSIDE", "path": l.outside_dir.display().to_string() }),
        ),
        Some(&l.wt),
    );
    assert_refused("absolute Grep", &out);

    let out = execute_grep(
        &inv(
            "Grep",
            json!({ "pattern": "OUTSIDE", "path": "../outside" }),
        ),
        Some(&l.wt),
    );
    assert_refused("`..` Grep", &out);
}

#[cfg(unix)]
#[test]
fn grep_does_not_follow_a_symlink_out_of_the_worktree() {
    let l = layout();
    symlink(&l.outside_dir, &l.wt.join("vendor"));
    symlink(&l.outside_file, &l.wt.join("src/innocent.txt"));
    let out = execute_grep(&inv("Grep", json!({ "pattern": "OUTSIDE" })), Some(&l.wt));
    assert!(
        !out.contains(OUTSIDE_SECRET),
        "Grep followed a symlink out of the jail: {out}"
    );
}

#[test]
fn ls_of_a_directory_outside_the_worktree_is_refused() {
    let l = layout();
    let out = execute_ls(
        &inv("LS", json!({ "path": l.outside_dir.display().to_string() })),
        Some(&l.wt),
    );
    assert_refused("absolute LS", &out);
    assert!(!out.contains("secret.txt"), "{out}");

    let out = execute_ls(&inv("LS", json!({ "path": "../outside" })), Some(&l.wt));
    assert_refused("`..` LS", &out);
    assert!(!out.contains("secret.txt"), "{out}");
}

#[test]
fn reads_without_a_worktree_are_refused() {
    let l = layout();
    let out = execute_read(
        &inv(
            "Read",
            json!({ "file_path": l.outside_file.display().to_string() }),
        ),
        None,
    );
    assert_refused("Read with no jail root", &out);
}

#[test]
fn reads_inside_the_worktree_work() {
    let l = layout();
    let read = execute_read(
        &inv("Read", json!({ "file_path": "src/lib.rs" })),
        Some(&l.wt),
    );
    assert!(read.contains("pub fn inside"), "{read}");

    let abs = l.wt.join("src/lib.rs").display().to_string();
    let read_abs = execute_read(&inv("Read", json!({ "file_path": abs })), Some(&l.wt));
    assert!(
        read_abs.contains("pub fn inside"),
        "an absolute path inside the jail is fine: {read_abs}"
    );

    let glob = execute_glob(&inv("Glob", json!({ "pattern": "src/*.rs" })), Some(&l.wt));
    assert!(glob.contains("lib.rs"), "{glob}");

    let grep = execute_grep(&inv("Grep", json!({ "pattern": "inside" })), Some(&l.wt));
    assert!(grep.contains("lib.rs"), "{grep}");

    let ls = execute_ls(&inv("LS", json!({ "path": "src" })), Some(&l.wt));
    assert!(ls.contains("lib.rs"), "{ls}");
}

#[test]
fn a_declared_read_folder_is_readable_and_logged_without_its_host_path() {
    let l = layout();
    let sibling = l.outside_dir.clone();
    let jail = ReadJail::new(Some(&l.wt), std::slice::from_ref(&sibling));
    let out = read_jail::execute_read(
        &inv(
            "Read",
            json!({ "file_path": l.outside_file.display().to_string() }),
        ),
        &jail,
    );
    assert_eq!(out.verdict, super::ReadVerdict::Allowed, "{}", out.text);
    assert!(out.text.contains(OUTSIDE_SECRET), "{}", out.text);
    assert_eq!(out.path, "read-folder:outside/secret.txt");

    let glob = read_jail::execute_glob(
        &inv(
            "Glob",
            json!({ "pattern": format!("{}/*.txt", sibling.display()) }),
        ),
        &jail,
    );
    assert!(
        glob.text.contains(&l.outside_file.display().to_string()),
        "a read folder's matches are shown by a path the model can pass back: {}",
        glob.text
    );
}

#[tokio::test]
async fn a_refused_read_reaches_the_model_as_a_tool_error_through_the_driver() {
    use std::sync::Arc;
    let l = layout();
    let driver = super::ApiAgentDriver::new(
        Arc::new(super::AnthropicNoopCompleter),
        "claude-sonnet-5",
        l.wt.clone(),
    );
    let role = camerata_core::Role {
        name: "jail".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    let (text, denial) = super::execute_tool(
        &driver,
        &role,
        &inv(
            "Read",
            json!({ "file_path": l.outside_file.display().to_string() }),
        ),
    )
    .await;
    assert_refused("driver Read", &text);
    assert!(
        denial.is_none(),
        "a refused read is a tool error, not a write denial"
    );
}

#[test]
fn a_read_folder_does_not_widen_the_write_jail() {
    let l = layout();
    let driver = super::ApiAgentDriver::new(
        std::sync::Arc::new(super::AnthropicNoopCompleter),
        "claude-sonnet-5",
        l.wt.clone(),
    )
    .with_read_dirs(vec![l.outside_dir.clone()]);
    let role = camerata_core::Role {
        name: "jail".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    let target = l.outside_dir.join("planted.txt");
    let (text, denial) = super::execute_gated_write(
        &driver,
        &role,
        &inv(
            "gated_write",
            json!({ "path": target.display().to_string(), "content": "x" }),
        ),
    );
    assert!(denial.is_some(), "{text}");
    assert!(!target.exists(), "nothing was written into the read folder");
}
