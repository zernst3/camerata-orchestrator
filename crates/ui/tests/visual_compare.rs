#![allow(clippy::unwrap_used)]
//! Unit tests of the visual-regression comparison, with small synthetic images (no Chrome). They
//! run in every default `cargo test`, so CI exercises the comparison even though the pixel
//! baselines themselves (`visual.rs`, feature `visual-baselines`) are macOS-only.

use std::path::{Path, PathBuf};

use image::RgbaImage;

#[path = "visual/compare.rs"]
mod compare;

use compare::*;

// ---------------------------------------------------------------------------------------------
// Unit tests of the comparison, with small synthetic images (no Chrome)
// ---------------------------------------------------------------------------------------------

fn png_of(img: &RgbaImage) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
    buf.into_inner()
}

fn flat(w: u32, h: u32, v: u8) -> RgbaImage {
    RgbaImage::from_pixel(w, h, image::Rgba([v, v, v, 255]))
}

const STRICT: Tolerance = Tolerance {
    max_channel_delta: 2,
    max_changed_pixels: 3,
};

#[test]
fn identical_images_match() {
    let img = png_of(&flat(10, 10, 200));
    assert_eq!(compare("tree/x", Some(&img), &img, STRICT), Verdict::Match);
}

#[test]
fn noise_inside_the_tolerance_matches() {
    let base = flat(10, 10, 200);
    let mut act = base.clone();
    // Three pixels off by 2 (not past the channel tolerance) and three off by 9 (past it, but
    // only three pixels, which the count allows).
    for x in 0..3 {
        act.put_pixel(x, 0, image::Rgba([202, 200, 200, 255]));
        act.put_pixel(x, 1, image::Rgba([209, 200, 200, 255]));
    }
    assert_eq!(
        compare("tree/x", Some(&png_of(&base)), &png_of(&act), STRICT),
        Verdict::Match
    );
}

#[test]
fn a_change_past_the_tolerance_is_reported_with_its_size() {
    let base = flat(10, 10, 200);
    let mut act = base.clone();
    for x in 0..4 {
        act.put_pixel(x, 0, image::Rgba([230, 200, 200, 255]));
    }
    assert_eq!(
        compare("tree/x", Some(&png_of(&base)), &png_of(&act), STRICT),
        Verdict::Changed {
            changed_pixels: 4,
            max_delta: 30
        }
    );
}

#[test]
fn a_changed_image_size_is_a_failure_not_a_match() {
    let v = compare(
        "tree",
        Some(&png_of(&flat(10, 10, 200))),
        &png_of(&flat(10, 11, 200)),
        STRICT,
    );
    assert_eq!(
        v,
        Verdict::SizeChanged {
            baseline: (10, 10),
            actual: (10, 11)
        }
    );
}

#[test]
fn a_missing_baseline_is_a_failure() {
    assert_eq!(
        compare("tree/x", None, &png_of(&flat(4, 4, 0)), STRICT),
        Verdict::MissingBaseline
    );
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("camerata-visual-{}-{name}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(dir: &Path, name: &str, img: &RgbaImage) {
    std::fs::write(dir.join(format!("{name}.png")), png_of(img)).unwrap();
}

fn two_states() -> Vec<(String, String)> {
    vec![
        ("tree".to_string(), "markers".to_string()),
        ("shell".to_string(), "empty".to_string()),
    ]
}

/// Write an actual screenshot of every width for both states.
fn write_actuals(dir: &Path, img: &RgbaImage) {
    for (s, st) in two_states() {
        for w in WIDTHS {
            write_png(dir, &stem(&s, &st, w), img);
        }
    }
}

#[test]
fn an_empty_baselines_folder_fails_every_state_as_new() {
    let baselines = scratch("empty-baselines");
    let actual = scratch("empty-baselines-actual");
    write_actuals(&actual, &flat(8, 8, 90));
    let failures = check_states(&baselines, &actual, &two_states(), STRICT, None);
    assert_eq!(failures.len(), 4, "{failures:?}");
    for f in &failures {
        assert!(
            f.why.contains("run scripts/visual-approve.sh after review"),
            "{f:?}"
        );
    }
    assert!(failures.iter().any(|f| f.what == "tree/markers @1000"));
    assert!(failures.iter().any(|f| f.what == "shell/empty @1440"));
}

#[test]
fn matching_baselines_pass_and_a_removed_state_leaves_a_stale_failure() {
    let baselines = scratch("stale-baselines");
    let actual = scratch("stale-baselines-actual");
    let img = flat(8, 8, 90);
    write_actuals(&baselines, &img);
    write_actuals(&actual, &img);
    assert!(check_states(&baselines, &actual, &two_states(), STRICT, None).is_empty());

    write_png(&baselines, "queue-gone-1440", &img);
    let failures = check_states(&baselines, &actual, &two_states(), STRICT, None);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].what, "queue-gone-1440.png");
    assert!(failures[0].why.contains("stale baseline"), "{failures:?}");
}

#[test]
fn a_changed_state_names_itself_and_writes_actual_and_diff_images() {
    let baselines = scratch("changed-baselines");
    let actual = scratch("changed-actual");
    let diffs = scratch("changed-diffs");
    write_actuals(&baselines, &flat(8, 8, 90));
    write_actuals(&actual, &flat(8, 8, 90));
    let mut changed = flat(8, 8, 90);
    for x in 0..8 {
        changed.put_pixel(x, 0, image::Rgba([250, 10, 10, 255]));
    }
    write_png(&actual, "tree-markers-1000", &changed);
    let failures = check_states(&baselines, &actual, &two_states(), STRICT, Some(&diffs));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].what, "tree/markers @1000");
    assert!(failures[0].why.contains("8 pixels changed"), "{failures:?}");
    assert!(diffs.join("tree-markers-1000-actual.png").exists());
    let diff = decode(&std::fs::read(diffs.join("tree-markers-1000-diff.png")).unwrap()).unwrap();
    assert_eq!(*diff.get_pixel(0, 0), image::Rgba([255, 0, 255, 255]));
    assert_ne!(*diff.get_pixel(0, 5), image::Rgba([255, 0, 255, 255]));
}

#[test]
fn a_chrome_major_or_platform_mismatch_fails_with_the_approve_instruction() {
    let recorded = version_file_content("156.0.8078.80");
    let plat = platform();
    assert!(check_environment(Some(&recorded), "156.0.9999.1", &plat).is_ok());
    let major_err = check_environment(Some(&recorded), "157.0.1.1", &plat).unwrap_err();
    assert!(major_err.contains("major version differs"), "{major_err}");
    assert!(
        major_err.contains("scripts/visual-approve.sh"),
        "{major_err}"
    );
    let plat_err = check_environment(Some(&recorded), "156.0.1.1", "linux-x86_64").unwrap_err();
    assert!(plat_err.contains("platform differs"), "{plat_err}");
    let missing = check_environment(None, "156.0.1.1", &plat).unwrap_err();
    assert!(missing.contains("CHROME_VERSION is missing"), "{missing}");
}

#[test]
fn chrome_version_is_parsed_from_its_banner() {
    assert_eq!(
        parse_chrome_version("Google Chrome 156.0.8078.80 \n").as_deref(),
        Some("156.0.8078.80")
    );
    assert_eq!(parse_chrome_version("no version here"), None);
}

fn page(height: u32, content_rows: &[u32]) -> RgbaImage {
    // A page on the flat test background with one content pixel on each of `content_rows`.
    let [r, g, b] = FLAT_BG;
    let mut img = RgbaImage::from_pixel(16, height, image::Rgba([r, g, b, 255]));
    for y in content_rows {
        img.put_pixel(3, *y, image::Rgba([250, 10, 120, 255]));
    }
    img
}

#[test]
fn content_height_is_the_last_row_off_the_flat_background() {
    assert_eq!(content_height(&page(3000, &[])), 0);
    assert_eq!(content_height(&page(3000, &[7])), 8);
    assert_eq!(content_height(&page(3000, &[7, 2000])), 2001);
}

#[test]
fn a_page_is_kept_as_tall_as_its_content_between_the_floor_and_the_cap() {
    assert_eq!(kept_height(&page(3000, &[7]), "queue/x"), CAPTURE_HEIGHT);
    assert_eq!(kept_height(&page(3000, &[2000]), "queue/x"), 2001);
    assert_eq!(
        kept_height(
            &page(MAX_CAPTURE_HEIGHT + 100, &[MAX_CAPTURE_HEIGHT + 50]),
            "tree"
        ),
        MAX_CAPTURE_HEIGHT
    );
}

#[test]
fn the_shell_screen_is_always_cut_at_the_floor() {
    assert_eq!(
        kept_height(&page(3000, &[2000]), "shell/empty"),
        CAPTURE_HEIGHT
    );
}

#[test]
fn content_below_the_old_1400_cut_is_compared() {
    // The same page with one pixel changed at row 1900 must not match: that row used to sit in
    // the band under the capture where a change passed unseen.
    let base = page(3000, &[10, 1900]);
    let mut act = base.clone();
    act.put_pixel(3, 1900, image::Rgba([0, 255, 0, 255]));
    let stored = normalize_screenshot("queue/x", &png_of(&base)).unwrap();
    let v = compare(
        "queue/x",
        Some(&stored),
        &png_of(&act),
        Tolerance {
            max_channel_delta: 0,
            max_changed_pixels: 0,
        },
    );
    assert!(
        matches!(
            v,
            Verdict::Changed {
                changed_pixels: 1,
                ..
            }
        ),
        "{v:?}"
    );
}

#[test]
fn normalizing_keeps_the_content_rows_pixel_for_pixel_and_drops_the_rest() {
    let img = page(3000, &[7, 2500]);
    let out = decode(&normalize_screenshot("queue/x", &png_of(&img)).unwrap()).unwrap();
    assert_eq!(out.dimensions(), (16, 2501));
    assert_eq!(*out.get_pixel(3, 7), image::Rgba([250, 10, 120, 255]));
    assert_eq!(*out.get_pixel(3, 2500), image::Rgba([250, 10, 120, 255]));
    let shell = decode(&normalize_screenshot("shell/empty", &png_of(&img)).unwrap()).unwrap();
    assert_eq!(shell.dimensions(), (16, CAPTURE_HEIGHT));
}

#[test]
fn the_flat_background_colour_matches_the_injected_style() {
    let [r, g, b] = FLAT_BG;
    assert!(FLAT_BACKGROUND_STYLE.contains(&format!("#{r:02x}{g:02x}{b:02x}")));
}

#[test]
fn a_screenshot_shorter_than_the_capture_height_is_refused() {
    assert!(normalize_screenshot("tree/x", &png_of(&flat(16, 100, 0))).is_err());
}

const HTML: &str = "<html><head><title>t</title></head><body>x</body></html>";

#[test]
fn every_page_gets_its_animations_frozen_inside_the_head() {
    for screen in ["shell", "tree"] {
        let page = prepare_page(HTML, screen);
        assert_eq!(page.matches("animation: none").count(), 1, "{screen}");
        assert!(page.find("animation: none").unwrap() < page.find("</head>").unwrap());
    }
    assert!(prepare_page("<p>no head</p>", "tree").starts_with("<style>"));
}

#[test]
fn a_page_off_the_shell_screen_gets_a_flat_background() {
    let page = prepare_page(HTML, "queue");
    assert!(page.contains("background-image: none !important"), "{page}");
    assert!(page.find("background-image: none").unwrap() < page.find("</head>").unwrap());
}

#[test]
fn the_shell_screen_keeps_the_real_background() {
    let page = prepare_page(HTML, "shell");
    assert!(!page.contains("background-image"), "{page}");
    assert!(!page.contains("background:"), "{page}");
}

fn probe_image(width: u32, window: u32, painted: u32) -> RgbaImage {
    // What the 100vh probe screenshot looks like: PROBE_COLOUR down to the viewport's last row,
    // then the unpainted (white) bar below it.
    RgbaImage::from_fn(width, window, |_, y| {
        if y < painted {
            image::Rgba(PROBE_COLOUR)
        } else {
            image::Rgba([255, 255, 255, 255])
        }
    })
}

#[test]
fn the_window_is_the_captured_height_plus_the_measured_bar() {
    assert_eq!(window_height(CAPTURE_HEIGHT, 129), CAPTURE_HEIGHT + 129);
    assert_eq!(window_height(1400, 0), 1400);
}

#[test]
fn the_bar_is_the_window_minus_the_rows_the_probe_painted() {
    let img = probe_image(8, 2000, 1871);
    assert_eq!(painted_rows(&img), 1871);
    assert_eq!(bar_from_probe(2000, painted_rows(&img)), 129);
}

#[test]
fn the_probe_is_exact_only_when_its_last_painted_row_is_the_capture_s_last_row() {
    let exact = probe_image(8, window_height(CAPTURE_HEIGHT, 129), CAPTURE_HEIGHT);
    assert_eq!(painted_rows(&exact), CAPTURE_HEIGHT);
    // A viewport taller than the capture (the old 1700 window) hides rows below the crop.
    let too_tall = probe_image(8, 1700, 1571);
    assert_ne!(painted_rows(&too_tall), CAPTURE_HEIGHT);
    // A viewport shorter than the capture leaves unpainted rows inside it.
    let too_short = probe_image(8, 1529, 1398);
    assert_ne!(painted_rows(&too_short), CAPTURE_HEIGHT);
}

#[test]
fn the_probe_page_is_one_viewport_tall_block_of_the_probe_colour() {
    assert!(PROBE_HTML.contains("height: 100vh"));
    let [r, g, b, _] = PROBE_COLOUR;
    assert!(PROBE_HTML.contains(&format!("rgb({r}, {g}, {b})")));
}

fn tall_page_with_content_at(row: u32) -> Vec<u8> {
    png_of(&page(MAX_CAPTURE_HEIGHT, &[10, row]))
}

#[test]
fn a_page_whose_content_reaches_the_cap_is_flagged() {
    let (_, reaches) = compare_shot(
        "queue/x",
        None,
        &tall_page_with_content_at(MAX_CAPTURE_HEIGHT - 1),
        STRICT,
    );
    assert!(reaches);
    let (_, reaches) = compare_shot(
        "queue/x",
        None,
        &tall_page_with_content_at(MAX_CAPTURE_HEIGHT - CAP_MARGIN - 2),
        STRICT,
    );
    assert!(!reaches);
}

#[test]
fn a_state_that_reaches_the_cap_must_be_named_in_capped() {
    let why = cap_failure("queue/30k", true).unwrap();
    assert!(why.contains("grows past"), "{why}");
    assert!(why.contains("CAPPED"), "{why}");
    assert_eq!(cap_failure("tree/markers", false), None);
}

#[test]
fn a_capped_state_that_no_longer_reaches_the_cap_must_leave_capped() {
    assert!(CAPPED.contains(&"tree/30k"));
    assert_eq!(cap_failure("tree/30k", true), None);
    let why = cap_failure("tree/30k", false).unwrap();
    assert!(why.contains("no longer reaches"), "{why}");
}

#[test]
fn the_shell_show_next_and_overlay_captures_are_short_and_never_judged_against_the_cap() {
    for id in ["shell/empty", "tree/30k-show-next", "detail/viewer-commit"] {
        assert_eq!(
            kept_height(&page(3000, &[2000]), id),
            CAPTURE_HEIGHT,
            "{id}"
        );
        assert_eq!(cap_failure(id, true), None, "{id}");
        assert_eq!(cap_failure(id, false), None, "{id}");
    }
}

#[test]
fn check_states_fails_an_unnamed_state_that_grows_past_the_cap() {
    let baselines = scratch("cap-baselines");
    let actual = scratch("cap-actual");
    let states = vec![("queue".to_string(), "30k".to_string())];
    for w in WIDTHS {
        let shot = tall_page_with_content_at(MAX_CAPTURE_HEIGHT - 1);
        std::fs::write(actual.join(format!("queue-30k-{w}.png")), &shot).unwrap();
        let stored = normalize_screenshot("queue/30k", &shot).unwrap();
        std::fs::write(baselines.join(format!("queue-30k-{w}.png")), stored).unwrap();
    }
    let failures = check_states(&baselines, &actual, &states, STRICT, None);
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(failures[0].why.contains("grows past"), "{failures:?}");
}

#[test]
fn the_show_next_variant_slides_the_page_so_the_show_next_row_is_in_view() {
    let page = scrolled_to_show_next(HTML);
    let head = page.find("</head>").unwrap();
    let script = page.find("<script>").unwrap();
    assert!(page.find("overflow: hidden").unwrap() < head);
    assert!(page.contains("translateY"));
    assert!(page.contains("Show next"));
    // `top` is a read-only global in a page: assigning it is silently ignored.
    assert!(!page.contains("var top"));
    assert!(script > head && script < page.find("</body>").unwrap());
    assert!(page.contains(&format!("height: {CAPTURE_HEIGHT}px")));
}
