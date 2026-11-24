//! Tests for the review room (design sections 1 and 2).

use super::room::{Room, RoomError, MAX_READ_LINES, OUT_DIR};
use super::Piece;

fn f(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn piece(files: &[&str]) -> Piece {
    Piece {
        key: "acme/app#1:00000000".into(),
        unit: "acme/app".into(),
        files: files.iter().map(|s| s.to_string()).collect(),
    }
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn repo() -> Vec<(String, String)> {
    vec![
        f(
            "src/main.rs",
            &format!(
                "mod util;\nfn main() {{ util::go(); }}\n// {}\n",
                "padding ".repeat(30)
            ),
        ),
        f("src/util.rs", "pub fn go() {}\n"),
        f("src/other.rs", "pub fn other() {}\n"),
        f("README.md", "# readme\n"),
    ]
}

fn room() -> Room {
    Room::build(&piece(&["src/main.rs"]), &repo(), "# checklist\n").room
}

#[test]
fn the_room_holds_the_piece_the_context_files_and_the_four_harness_files() {
    let r = room();
    let paths = r.paths();
    for expected in [
        "src/main.rs",
        "_camerata/context/tree.txt",
        "_camerata/context/neighbours.md",
        "_camerata/context/context-files.txt",
        "_camerata/checklist.md",
    ] {
        assert!(
            paths.iter().any(|p| p == expected),
            "missing {expected}: {paths:?}"
        );
    }
    assert_eq!(r.piece_files(), ["src/main.rs".to_string()]);
    assert!(
        r.context_files().iter().any(|p| p == "src/util.rs"),
        "{:?}",
        r.context_files()
    );
    assert!(
        !paths.iter().any(|p| p == "src/other.rs"),
        "an unrelated file is not served"
    );
}

#[test]
fn the_tree_lists_every_file_of_the_repository() {
    let r = room();
    let tree = r
        .read("_camerata/context/tree.txt", 1, 100)
        .expect("tree reads");
    for p in ["src/main.rs", "src/util.rs", "src/other.rs", "README.md"] {
        assert!(tree.text.contains(p), "{p} not in the tree: {}", tree.text);
    }
}

#[test]
fn a_path_not_in_the_room_does_not_exist() {
    let r = room();
    for path in [
        "src/other.rs",
        "/etc/passwd",
        "/src/main.rs",
        "../secrets.txt",
        "src/../src/other.rs",
        "src/main.rs/../other.rs",
        "nope.rs",
    ] {
        assert!(!r.contains(path), "{path} must not exist");
        assert!(
            matches!(r.read(path, 1, 10), Err(RoomError::NotFound(_))),
            "{path} read"
        );
    }
    assert!(
        r.contains("./src/main.rs"),
        "a leading ./ names the same file"
    );
    assert!(r.read("./src/main.rs", 1, 10).is_ok());
}

#[test]
fn a_write_outside_the_out_folder_is_refused() {
    let r = room();
    for path in [
        "src/main.rs",
        "notes.txt",
        "_camerata/checklist.md",
        "_camerata/out",
        "_camerata/out/../checklist.md",
        "/tmp/x",
        "_camerata/outside/x",
    ] {
        assert!(r.write(path, "x").is_err(), "{path} must be refused");
    }
    assert!(r.outputs().is_empty());
    assert!(r
        .read("src/main.rs", 1, 10)
        .expect("reads")
        .text
        .contains("mod util"));
}

#[test]
fn a_write_under_the_out_folder_is_stored_and_can_be_read_back() {
    let r = room();
    r.write(&format!("{OUT_DIR}problems-1.json"), "[]")
        .expect("accepted");
    r.write(&format!("{OUT_DIR}problems-1.json"), "[1]")
        .expect("replaced");
    assert_eq!(
        r.outputs()
            .get("_camerata/out/problems-1.json")
            .map(String::as_str),
        Some("[1]")
    );
    assert!(r.read("_camerata/out/problems-1.json", 1, 5).is_ok());
}

#[test]
fn read_returns_numbered_lines_and_how_many_it_returned() {
    let big = numbered(5_000);
    let r = Room::build(&piece(&["big.rs"]), &[f("big.rs", &big)], "").room;
    let one = r.read("big.rs", 1, 3).expect("reads");
    assert_eq!(one.lines_returned, 3);
    assert_eq!(one.first_line, 1);
    assert_eq!(one.total_lines, 5_000);
    assert!(one.text.contains("1\tline 1"), "{:?}", one.text);
    assert!(one.text.contains("3\tline 3"));
    assert!(!one.text.contains("line 4"));

    let cut = r.read("big.rs", 1, 9_999).expect("reads");
    assert_eq!(cut.lines_returned, MAX_READ_LINES);
    let tail = r.read("big.rs", 4_999, 50).expect("reads");
    assert_eq!(tail.lines_returned, 2);
    let past = r.read("big.rs", 6_000, 5).expect("reads");
    assert_eq!(past.lines_returned, 0);
}

#[test]
fn a_secret_value_in_a_source_file_is_absent_from_every_served_byte() {
    let secret = "sk_live_4eC39HqLyjWDarjtT1zdp7dcAbCdEfGhIjKl";
    let src = format!("const KEY = \"{secret}\";\nfn main() {{}}\n");
    let files = vec![
        f("src/main.rs", &src),
        f("src/util.rs", &format!("// {secret}\npub fn go() {{}}\n")),
    ];
    let redacted = crate::onboard::redact::files_for_model(&files);
    let r = Room::build(&piece(&["src/main.rs"]), &redacted, "# checklist\n").room;
    assert!(
        r.paths().len() >= 5,
        "the room serves files: {:?}",
        r.paths()
    );
    assert!(
        r.read("src/main.rs", 1, 10)
            .expect("reads")
            .text
            .contains("fn main"),
        "the room serves the redacted source"
    );
    for path in r.paths() {
        let all = r.read(&path, 1, MAX_READ_LINES).expect("reads");
        assert!(!all.text.contains(secret), "{path} serves the secret");
    }
    let hits = r.grep("sk_live", None).expect("grep runs");
    assert!(hits.iter().all(|h| !h.text.contains(secret)));
    let dir = tempfile::tempdir().expect("tempdir");
    r.materialize(dir.path()).expect("materializes");
    for path in r.paths() {
        let on_disk = std::fs::read_to_string(dir.path().join(&path)).expect("file written");
        assert!(!on_disk.contains(secret), "{path} on disk holds the secret");
    }
}

#[test]
fn context_files_stay_within_thirty_percent_of_the_piece_and_the_rest_are_outline_only() {
    // The piece is 1,000 bytes; two imported files of 200 and 300 bytes fit the 300 byte budget
    // only one at a time; a third of 400 bytes never fits.
    let main = format!("mod a;\nmod b;\nmod c;\n{}", "x".repeat(985));
    let files = vec![
        f("src/main.rs", &main),
        f("src/a.rs", &format!("pub fn a() {{}}\n{}", "y".repeat(184))),
        f("src/b.rs", &format!("pub fn b() {{}}\n{}", "y".repeat(284))),
        f("src/c.rs", &format!("pub fn c() {{}}\n{}", "y".repeat(384))),
    ];
    let piece_bytes = main.len();
    let r = Room::build(&piece(&["src/main.rs"]), &files, "").room;
    let used: usize = r
        .context_files()
        .iter()
        .map(|p| {
            files
                .iter()
                .find(|(q, _)| q == p)
                .map_or(0, |(_, c)| c.len())
        })
        .sum();
    assert!(
        used * 100 <= piece_bytes * 30,
        "context takes {used} of {piece_bytes}"
    );
    assert!(!r.context_files().is_empty(), "something fits");
    assert!(
        !r.context_files().iter().any(|p| p == "src/c.rs"),
        "c.rs is over budget"
    );
    let neighbours = r
        .read("_camerata/context/neighbours.md", 1, 200)
        .expect("reads")
        .text;
    assert!(
        neighbours.contains("src/c.rs"),
        "an over-budget neighbour is still outlined"
    );
    assert!(
        neighbours.contains("pub fn c") || neighbours.contains("c"),
        "{neighbours}"
    );
    let listing = r
        .read("_camerata/context/context-files.txt", 1, 50)
        .expect("reads")
        .text;
    for p in r.context_files() {
        assert!(listing.contains(p.as_str()), "{p} not listed: {listing}");
    }
}

#[test]
fn glob_grep_and_ls_are_served_from_the_room_only() {
    let r = room();
    assert_eq!(
        r.glob("src/*.rs"),
        vec!["src/main.rs".to_string(), "src/util.rs".to_string()]
    );
    assert!(r.glob("**/other.rs").is_empty());
    let hits = r.grep("util", None).expect("grep runs");
    assert!(hits.iter().any(|h| h.path == "src/main.rs" && h.line == 1));
    assert!(hits.iter().all(|h| h.path != "src/other.rs"));
    assert!(matches!(r.grep("(", None), Err(RoomError::Invalid(_))));
    let root = r.ls("").expect("root lists");
    assert!(root.iter().any(|e| e == "src/"), "{root:?}");
    assert!(root.iter().any(|e| e == "_camerata/"), "{root:?}");
    assert!(matches!(r.ls("../"), Err(RoomError::NotFound(_))));
    assert!(matches!(r.ls("/etc"), Err(RoomError::NotFound(_))));
}

#[test]
fn materialize_writes_exactly_the_rooms_files() {
    let r = room();
    r.write("_camerata/out/card.md", "card").expect("accepted");
    let dir = tempfile::tempdir().expect("tempdir");
    let mut written = r.materialize(dir.path()).expect("materializes");
    written.sort();
    assert_eq!(written, r.paths());
    let mut on_disk = Vec::new();
    let mut stack = vec![dir.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("reads dir") {
            let e = e.expect("entry");
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                on_disk.push(
                    e.path()
                        .strip_prefix(dir.path())
                        .expect("under dir")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    on_disk.sort();
    assert_eq!(on_disk, r.paths());
    assert!(
        dir.path().join("_camerata/out").is_dir(),
        "the out folder exists"
    );
}

#[test]
fn only_keeps_the_named_piece_files_and_the_harness_files() {
    let files = vec![f("a.rs", "fn a() {}\n"), f("b.rs", "fn b() {}\n")];
    let r = Room::build(&piece(&["a.rs", "b.rs"]), &files, "# c\n").room;
    let again = r.only(&["b.rs".to_string()]);
    assert_eq!(again.piece_files(), ["b.rs".to_string()]);
    assert!(!again.contains("a.rs"));
    assert!(again.contains("_camerata/checklist.md"));
}

#[test]
fn a_piece_file_missing_from_the_repository_is_reported_and_the_rest_are_served() {
    let built = Room::build(
        &piece(&["src/main.rs", "src/vanished.rs"]),
        &repo(),
        "# checklist\n",
    );
    assert_eq!(built.not_served.len(), 1, "{:?}", built.not_served);
    assert_eq!(built.not_served[0].path, "src/vanished.rs");
    assert!(
        built.not_served[0].reason.contains("not in the repository"),
        "{:?}",
        built.not_served[0]
    );
    // The file it could serve is a piece file with a line count; the missing one is not a piece
    // file of the room (it stays owed through `not_served`).
    assert_eq!(built.room.piece_files(), ["src/main.rs".to_string()]);
    let counts = built.room.piece_line_counts();
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0].0, "src/main.rs");
    assert_eq!(counts[0].1, 3);
    assert!(Room::build(&piece(&["src/main.rs"]), &repo(), "")
        .not_served
        .is_empty());
}

#[test]
fn a_client_file_under_the_rooms_own_folder_cannot_shadow_a_room_file() {
    let mut files = repo();
    files.push(f(
        "_camerata/checklist.md",
        "CLIENT FILE, NOT THE CHECKLIST\n",
    ));
    files.push(f("_camerata/out/problems-1.json", "[]"));
    files.push(f("_camerata/notes.rs", "pub fn notes() {}\n"));
    let built = Room::build(
        &piece(&["src/main.rs", "_camerata/notes.rs"]),
        &files,
        "# real checklist\n",
    );
    let served = built
        .room
        .read("_camerata/checklist.md", 1, 5)
        .expect("the room's own checklist");
    assert!(served.text.contains("real checklist"), "{}", served.text);
    assert!(!served.text.contains("CLIENT FILE"));
    // The piece file with a reserved name cannot be served, so it is reported and stays owed.
    assert_eq!(built.not_served.len(), 1, "{:?}", built.not_served);
    assert_eq!(built.not_served[0].path, "_camerata/notes.rs");
    assert!(built.not_served[0].reason.contains("_camerata/"));
    assert!(!built.room.contains("_camerata/notes.rs"));
    // The client's file under the output folder never appears as an output.
    assert!(built.room.outputs().is_empty());
    assert!(built
        .room
        .read("_camerata/out/problems-1.json", 1, 5)
        .is_err());
}

#[test]
fn extra_files_are_read_only_not_piece_files_and_cannot_replace_a_room_file() {
    let room = room()
        .with_extra_files(&[
            f("_camerata/joints/cards.md", "card of part 1\n"),
            f("_camerata/joints/problems.txt", "1 RULE src/a.rs title\n"),
        ])
        .expect("extras added");
    assert_eq!(
        room.extra_files(),
        [
            "_camerata/joints/cards.md".to_string(),
            "_camerata/joints/problems.txt".to_string()
        ]
    );
    assert!(room
        .read("_camerata/joints/cards.md", 1, 5)
        .expect("readable")
        .text
        .contains("card of part 1"));
    assert_eq!(room.piece_files(), ["src/main.rs".to_string()]);
    assert!(room.write("_camerata/joints/cards.md", "x").is_err());
    // The send-again room keeps them.
    assert!(room
        .only(&["src/main.rs".to_string()])
        .contains("_camerata/joints/cards.md"));
    for bad in [
        "_camerata/checklist.md",
        "src/main.rs",
        "_camerata/out/x.json",
        "/etc/passwd",
        "_camerata/../src/main.rs",
    ] {
        let err = super::Room::build(&piece(&["src/main.rs"]), &repo(), "")
            .room
            .with_extra_files(&[f(bad, "x")]);
        assert!(err.is_err(), "{bad} was accepted");
    }
}

#[test]
fn one_read_is_limited_in_bytes_and_a_cut_read_counts_only_the_lines_it_returned() {
    // 400 lines of 600 bytes: well under the line limit, far over the byte limit.
    let long: String = (1..=400)
        .map(|i| format!("{i} {}\n", "x".repeat(600)))
        .collect();
    let r = Room::build(&piece(&["wide.rs"]), &[f("wide.rs", &long)], "").room;
    let out = r.read("wide.rs", 1, 400).expect("reads");
    assert!(out.lines_returned < 400, "{}", out.lines_returned);
    assert!(out.lines_returned > 100, "{}", out.lines_returned);
    assert!(out.text.contains("byte limit"), "the cut is announced");
    let numbered_part = out.text.split("(output stopped").next().unwrap_or("");
    assert!(numbered_part.len() <= super::room::MAX_READ_BYTES);
    assert_eq!(
        out.text.lines().filter(|l| l.contains('\t')).count(),
        out.lines_returned
    );
    // Reading on from where it stopped works.
    let rest = r
        .read("wide.rs", 1 + out.lines_returned, 400)
        .expect("reads");
    assert!(rest.lines_returned > 0);
    // A line longer than the limit cannot be returned at all, and is not counted as read.
    let one_line = "y".repeat(super::room::MAX_READ_BYTES + 10);
    let r = Room::build(&piece(&["min.js"]), &[f("min.js", &one_line)], "").room;
    let none = r.read("min.js", 1, 10).expect("reads");
    assert_eq!(none.lines_returned, 0);
}

#[test]
fn the_number_of_files_the_agent_may_write_is_limited_but_a_rewrite_is_free() {
    let r = room();
    for i in 0..super::room::MAX_WRITE_FILES {
        r.write(&format!("{OUT_DIR}problems-{i}.json"), "[]")
            .expect("under the limit");
    }
    let over = r.write(&format!("{OUT_DIR}one-too-many.json"), "[]");
    assert!(matches!(over, Err(RoomError::Invalid(_))), "{over:?}");
    assert_eq!(r.outputs().len(), super::room::MAX_WRITE_FILES);
    r.write(&format!("{OUT_DIR}problems-0.json"), "[1]")
        .expect("replacing a file adds none");
    assert_eq!(
        r.outputs()
            .get(&format!("{OUT_DIR}problems-0.json"))
            .map(String::as_str),
        Some("[1]")
    );
}

#[test]
fn piece_line_counts_use_the_rule_the_read_tool_numbers_by() {
    let files = vec![
        f("a.rs", "one\ntwo\nthree"),
        f("b.rs", "x\n\n\n"),
        f("empty.rs", ""),
    ];
    let r = Room::build(&piece(&["a.rs", "b.rs", "empty.rs"]), &files, "").room;
    let counts = r.piece_line_counts();
    for (path, lines) in &counts {
        let read = r.read(path, 1, MAX_READ_LINES).expect("reads");
        assert_eq!(read.total_lines, *lines, "{path}");
    }
    assert_eq!(
        counts,
        vec![
            ("a.rs".to_string(), 3),
            ("b.rs".to_string(), 3),
            ("empty.rs".to_string(), 0)
        ]
    );
    assert!(r
        .piece_file_text("a.rs")
        .expect("piece file")
        .contains("three"));
    assert!(r.piece_file_text("_camerata/checklist.md").is_none());
}

#[test]
fn materialize_under_code_puts_the_repository_under_code_and_the_rooms_files_beside_it() {
    let files = vec![
        f("src/main.rs", "fn main() {}\n"),
        f(".claude/settings.json", "{}\n"),
        f(".mcp.json", "{}\n"),
    ];
    let r = Room::build(
        &piece(&["src/main.rs", ".claude/settings.json", ".mcp.json"]),
        &files,
        "# checklist\n",
    )
    .room;
    let dir = tempfile::tempdir().expect("tempdir");
    let written = r.materialize_under_code(dir.path()).expect("materializes");
    assert_eq!(written.len(), r.paths().len());
    for p in ["src/main.rs", ".claude/settings.json", ".mcp.json"] {
        assert!(dir.path().join("code").join(p).is_file(), "{p}");
        assert!(r.contains(p), "{p} is served");
    }
    assert!(dir.path().join("_camerata/checklist.md").is_file());
    assert!(dir.path().join("_camerata/out").is_dir());
    assert!(!dir.path().join(".claude").exists());
    assert!(!dir.path().join(".mcp.json").exists());
    assert!(!dir.path().join("src").exists());
}

#[test]
fn long_lines_are_found_by_character_count() {
    let long = "x".repeat(2_001);
    let ok = "y".repeat(2_000);
    let files = vec![f("a.rs", &format!("short\n{ok}\n{long}\n"))];
    let r = Room::build(&piece(&["a.rs"]), &files, "").room;
    assert_eq!(r.long_lines().get("a.rs"), Some(&vec![3]));
}

#[test]
fn long_lines_count_characters_not_bytes() {
    // 1,500 two-byte characters are 3,000 bytes but 1,500 characters: not a long line.
    let wide = "\u{e9}".repeat(1_500);
    let files = vec![f("a.rs", &format!("{wide}\n"))];
    let r = Room::build(&piece(&["a.rs"]), &files, "").room;
    assert!(r.long_lines().is_empty(), "{:?}", r.long_lines());
}

#[test]
fn disk_path_compares_the_rooms_folder_without_regard_to_case() {
    assert_eq!(Room::disk_path("_camerata/x.md"), "_camerata/x.md");
    assert_eq!(Room::disk_path("_CAMERATA/x.md"), "_CAMERATA/x.md");
    assert_eq!(Room::disk_path("_Camerata/out/y"), "_Camerata/out/y");
    assert_eq!(Room::disk_path("src/a.rs"), "code/src/a.rs");
    assert_eq!(Room::disk_path("_camerata_x/a"), "code/_camerata_x/a");
}

/// Round 1 item 11: a 10,000-character line is read in full by the room's Read (nothing is cut),
/// and the command line transport, whose own Read cuts a line over 2,000 characters, is told
/// about it (`long_lines`) so the file is sent again whole ("read in full or not at all").
/// A single line over the room's 100,000 byte page limit is not covered: see the report.
#[test]
fn a_ten_thousand_character_line_is_read_in_full() {
    let long = "z".repeat(10_000);
    let files = vec![f("a.rs", &format!("short\n{long}\nend\n"))];
    let r = Room::build(&piece(&["a.rs"]), &files, "").room;
    let read = r.read("a.rs", 1, 10).expect("reads");
    assert_eq!(read.lines_returned, 3);
    assert!(read.text.contains(&long), "the whole line is in the answer");
    assert_eq!(r.long_lines().get("a.rs"), Some(&vec![2]));
}

/// V1 item 32 (extends the item 31 invariant): the room serves exactly the files the walk read,
/// which are exactly the files `camerata count` bills, across the admitted text types. The
/// generated, over-limit and agreed-excluded files are not in it.
#[test]
fn the_room_serves_exactly_the_billed_set_including_the_admitted_text_types() {
    let dir = tempfile::tempdir().unwrap();
    let put = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    put("src/app.js", "const a = 1;\n");
    put("views/page.ejs", "<h1><%- body %></h1>\n");
    put("public/index.html", "<p>hi</p>\n");
    put("Makefile", "run:\n\tnode src/app.js\n");
    put("README.md", "# Title\n");
    put("styles/site.css", "body { color: red; }\n");
    put(
        "Migrations/Init.Designer.cs",
        "// <auto-generated />\nclass A {}\n",
    );
    put("docs/agreed.md", "# left out by agreement\n");
    put(
        "docs/huge.md",
        &"x".repeat(crate::onboard::files::MAX_FILE_BYTES + 1),
    );
    let scope =
        crate::onboard::scope::ScanScope::from_patterns(&["app/docs/agreed.md".to_string()])
            .unwrap()
            .for_repo("acme/app");
    let extracted = crate::onboard::files::read_local_repo_files(dir.path(), &scope).unwrap();

    let model_files = crate::onboard::redact::files_for_model(&extracted.files);
    let all: Vec<&str> = model_files.iter().map(|(p, _)| p.as_str()).collect();
    let room = Room::build(&piece(&all), &model_files, "").room;
    let mut served = room.paths();
    served.retain(|p| !p.starts_with("_camerata"));
    served.sort();
    let mut billed: Vec<String> = extracted.files.iter().map(|(p, _)| p.clone()).collect();
    billed.sort();
    assert_eq!(
        billed,
        [
            "Makefile",
            "README.md",
            "public/index.html",
            "src/app.js",
            "styles/site.css",
            "views/page.ejs"
        ]
    );
    assert_eq!(served, billed, "the room serves what is billed, no more");
    // The piece planner cuts the same set: every billed file is in exactly one piece.
    let pieces = super::pieces::plan_pieces(
        &super::pieces::units_for("acme/app", &model_files),
        &model_files,
        super::pieces::TARGET_BYTES,
        None,
    );
    let mut planned: Vec<String> = pieces.iter().flat_map(|p| p.files.clone()).collect();
    planned.sort();
    assert_eq!(planned, billed, "the pieces cover what is billed, no more");
    for left_out in [
        "Migrations/Init.Designer.cs",
        "docs/agreed.md",
        "docs/huge.md",
    ] {
        assert!(!room.contains(left_out), "{left_out} is not in the room");
    }
    // The count bills the same files.
    let counted = crate::onboard::count::count_repo(dir.path(), &scope).unwrap();
    assert_eq!(counted.total_files, billed.len());
}

// ─── the Edit rule of the room ───────────────────────────────────────────────────────────────

fn written(text: &str) -> Room {
    let r = room();
    r.write("_camerata/out/p.json", text).expect("write");
    r
}

#[test]
fn an_edit_replaces_the_one_match_and_needs_exactly_one() {
    let r = written("alpha beta gamma");
    r.edit("_camerata/out/p.json", "beta", "BETA").expect("edit");
    assert_eq!(r.outputs()["_camerata/out/p.json"], "alpha BETA gamma");
    r.edit("./_camerata/out/p.json", "gamma", "")
        .expect("a leading ./ names the same file, and new may be empty");
    assert_eq!(r.outputs()["_camerata/out/p.json"], "alpha BETA ");

    let r = written("one two one");
    assert!(matches!(
        r.edit("_camerata/out/p.json", "one", "1"),
        Err(RoomError::Invalid(m)) if m.contains("2 places")
    ));
    assert!(matches!(
        r.edit("_camerata/out/p.json", "three", "3"),
        Err(RoomError::Invalid(m)) if m.contains("was not found")
    ));
    assert_eq!(r.outputs()["_camerata/out/p.json"], "one two one");
}

#[test]
fn overlapping_matches_count_as_more_than_one() {
    let r = written("aaa");
    assert!(matches!(
        r.edit("_camerata/out/p.json", "aa", "b"),
        Err(RoomError::Invalid(m)) if m.contains("2 places")
    ));
    assert_eq!(r.outputs()["_camerata/out/p.json"], "aaa");
    // The same through a multi-byte character.
    let r = written("ééé");
    assert!(r.edit("_camerata/out/p.json", "éé", "x").is_err());
    r.edit("_camerata/out/p.json", "é", "x").expect_err("three matches");
    let r = written("aé");
    r.edit("_camerata/out/p.json", "é", "e").expect("one match");
    assert_eq!(r.outputs()["_camerata/out/p.json"], "ae");
}

#[test]
fn an_edit_outside_the_out_folder_or_of_an_unwritten_file_is_refused() {
    let r = written("text");
    for path in [
        "src/main.rs",
        "_camerata/checklist.md",
        "_camerata/context/tree.txt",
        "/tmp/p.json",
        "../p.json",
        "_camerata/out/",
        "_camerata/out/../../p.json",
    ] {
        assert!(
            matches!(r.edit(path, "e", "x"), Err(RoomError::OutsideOut(_))),
            "{path}"
        );
    }
    assert!(matches!(
        r.edit("_camerata/out/other.json", "e", "x"),
        Err(RoomError::NotFound(_))
    ));
    assert!(matches!(
        r.edit("_camerata/out/p.json", "", "x"),
        Err(RoomError::Invalid(_))
    ));
    assert!(matches!(
        r.edit("_camerata/out/p.json", "text", "text"),
        Err(RoomError::Invalid(_))
    ));
    assert_eq!(r.outputs()["_camerata/out/p.json"], "text");
    assert!(r.read("src/main.rs", 1, 3).expect("read").text.contains("mod util"));
}

#[test]
fn an_edit_cannot_grow_a_file_past_the_write_limit() {
    let r = written("x");
    let big = "y".repeat(super::room::MAX_WRITE_BYTES);
    assert!(matches!(
        r.edit("_camerata/out/p.json", "x", &big),
        Err(RoomError::Invalid(m)) if m.contains("at most")
    ));
    assert_eq!(r.outputs()["_camerata/out/p.json"], "x");
}

// ─── the plain read of the checklist ─────────────────────────────────────────────────────────

fn room_with_checklist(text: &str) -> Room {
    Room::build(&piece(&["src/main.rs"]), &repo(), text).room
}

#[test]
fn the_checklist_is_served_plain_byte_for_byte_whatever_its_line_ends() {
    for text in [
        "# Checklist\n\nline one\nline two\n",
        "no final line end",
        "crlf line\r\nsecond\r\n\r\nlast\r\n",
        "tab\there\n  indented\n\n\n",
        "",
    ] {
        let r = room_with_checklist(text);
        let out = r
            .read_plain("_camerata/checklist.md", 1, 100_000)
            .expect("plain read");
        assert_eq!(out.text, text, "{text:?}");
        assert_eq!(out.lines_returned, text.lines().count());
        assert_eq!(out.total_lines, text.lines().count());
    }
}

#[test]
fn the_plain_read_has_no_byte_limit_and_still_pages_by_lines() {
    let big: String = (1..=1_500).map(|i| format!("{i} {}\n", "x".repeat(150))).collect();
    assert!(big.len() > super::room::MAX_READ_BYTES);
    let r = room_with_checklist(&big);
    let whole = r
        .read_plain("./_camerata/checklist.md", 1, MAX_READ_LINES)
        .expect("whole");
    assert_eq!(whole.text, big, "past the byte limit of the numbered read");
    assert_eq!(whole.lines_returned, 1_500);
    // The numbered read of the same text does stop at the byte limit.
    let numbered = r.read("_camerata/checklist.md", 1, MAX_READ_LINES).expect("numbered");
    assert!(numbered.lines_returned < 1_500);

    let page = r.read_plain("_camerata/checklist.md", 3, 2).expect("page");
    let expected: String = big.split_inclusive('\n').skip(2).take(2).collect();
    assert_eq!((page.text, page.first_line, page.lines_returned), (expected, 3, 2));
    let past = r.read_plain("_camerata/checklist.md", 2_000, 5).expect("past end");
    assert_eq!((past.text.as_str(), past.lines_returned, past.total_lines), ("", 0, 1_500));
}

#[test]
fn the_plain_read_serves_no_file_but_the_checklist() {
    let r = room();
    for path in [
        "src/main.rs",
        "_camerata/context/tree.txt",
        "_camerata/context/neighbours.md",
        "_Camerata/Checklist.md",
        "_camerata/out/x.json",
    ] {
        assert!(r.read_plain(path, 1, 10).is_err(), "{path}");
    }
    assert!(matches!(
        r.read_plain("../_camerata/checklist.md", 1, 10),
        Err(RoomError::Invalid(_))
    ));
}
