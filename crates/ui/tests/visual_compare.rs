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
    assert_eq!(compare(Some(&img), &img, STRICT), Verdict::Match);
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
        compare(Some(&png_of(&base)), &png_of(&act), STRICT),
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
        compare(Some(&png_of(&base)), &png_of(&act), STRICT),
        Verdict::Changed {
            changed_pixels: 4,
            max_delta: 30
        }
    );
}

#[test]
fn a_changed_image_size_is_a_failure_not_a_match() {
    let v = compare(
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
        compare(None, &png_of(&flat(4, 4, 0)), STRICT),
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

#[test]
fn normalizing_keeps_the_top_rows_pixel_for_pixel_and_drops_the_rest() {
    let mut img = flat(16, CAPTURE_HEIGHT + 50, 40);
    img.put_pixel(3, 7, image::Rgba([250, 10, 120, 255]));
    img.put_pixel(3, CAPTURE_HEIGHT + 10, image::Rgba([1, 2, 3, 255]));
    let out = decode(&normalize_screenshot(&png_of(&img)).unwrap()).unwrap();
    assert_eq!(out.dimensions(), (16, CAPTURE_HEIGHT));
    assert_eq!(*out.get_pixel(3, 7), image::Rgba([250, 10, 120, 255]));
    assert_eq!(
        *out.get_pixel(3, CAPTURE_HEIGHT - 1),
        image::Rgba([40, 40, 40, 255])
    );
}

#[test]
fn a_screenshot_shorter_than_the_capture_height_is_refused() {
    assert!(normalize_screenshot(&png_of(&flat(16, 100, 0))).is_err());
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
