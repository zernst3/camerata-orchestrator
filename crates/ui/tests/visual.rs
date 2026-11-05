#![allow(clippy::unwrap_used)]
//! Pixel-diff visual regression over the review gallery (review spec section 15, WO16, and U6).
//!
//! Every gallery state, at 1440 and 1000 wide, has an approved baseline PNG under
//! `tests/visual/baselines/<screen>-<state>-<width>.png`. `visual_regression_matches_baselines`
//! renders the gallery to a fresh folder under `target/`, screenshots every state with headless
//! Chrome, and fails on any change past `TOLERANCE`, listing each changed state and width. A
//! missing baseline, a baseline whose state no longer exists, a missing Chrome and a Chrome whose
//! major version differs from `baselines/CHROME_VERSION` are all failures, never skips.
//!
//! Run it with `scripts/visual-check.sh` (feature `visual-baselines`: the baselines are macOS
//! Chrome renders, so GitHub CI does not build this target). After a reviewed change, re-approve
//! with `scripts/visual-approve.sh [screen/state ...]`. The pure comparison it uses lives in
//! `visual/compare.rs`, unit-tested without Chrome by `visual_compare.rs` in every default run.
//!
//! # Determinism
//!
//! The gallery pages link web fonts from Google Fonts. A network fetch would make a screenshot
//! depend on connectivity, so Chrome runs with every hostname unresolvable
//! (`--host-resolver-rules`); `file://` pages are unaffected and the pages draw with the CSS
//! font stack's local fallbacks. The baselines are therefore the local-font rendering on the
//! platform recorded in `CHROME_VERSION`, not the Inter rendering an agent sees by hand.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[path = "visual/compare.rs"]
mod compare;

use compare::*;

/// The window-to-viewport bar this Chrome loses to its own chrome, in pixels, as measured by the
/// 100vh probe (`measure_bar`) at both widths: 0 on Chrome 156 (macOS arm64), where a screenshot's
/// viewport is exactly its window (probe windows of 1000 to 8600 tall painted all their rows).
/// Every state is screenshotted in a window `MAX_CAPTURE_HEIGHT + bar` tall (`window_height`), so
/// its viewport is the whole screenshot, and `kept_height` then keeps the rows down to the last
/// painted content (at least `CAPTURE_HEIGHT`, at most `MAX_CAPTURE_HEIGHT`). History: a 1700-tall
/// window with the top 1400 rows compared left content in rows 1400 to 1699 painted but never
/// compared, and a 1400-tall window would have cut the same content off instead; neither compared
/// the bottom of a tall screen. If a Chrome build gives a window a bar, the test fails and says
/// so: re-measure, update this number in review, then re-approve.
const EXPECTED_BAR: u32 = 0;

/// Chrome processes run at once. The machine also builds Rust, so a few, not all cores.
const WORKERS: usize = 4;

/// One Chrome launch must finish inside this.
const CHROME_TIMEOUT: Duration = Duration::from_secs(90);

/// Measured on 2026-11-05 (Chrome 156.0.8078.80, macOS arm64, 112 images: 56 states at 1440 and
/// 1000): after `FREEZE_STYLE` stops the loading skeletons animating, a clean render compared with
/// the approved baselines had 0 pixels with a channel delta above 0 in every image, in two clean
/// renders (`visual_measure_noise`; the `visual_approve` run before it and the
/// `visual_regression_matches_baselines` run after it agree). The rule is therefore exact: no
/// channel delta, no changed pixel. Before the freeze the three loading states drifted by up to
/// 164,000 pixels and a channel delta of 14 between renders; that was animation timing, not
/// rendering noise, and it is fixed at the source rather than hidden in a tolerance. Never
/// widen this to let a real change pass; if a different Chrome build adds anti-aliasing noise,
/// measure it again with `visual_measure_noise` and write the new numbers here.
const TOLERANCE: Tolerance = Tolerance {
    max_channel_delta: 0,
    max_changed_pixels: 0,
};

/// Every `(screen, state)` page under a rendered gallery folder, sorted.
fn gallery_states(gallery: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for screen in std::fs::read_dir(gallery).unwrap().flatten() {
        if !screen.file_type().unwrap().is_dir() {
            continue;
        }
        let screen_name = screen.file_name().to_string_lossy().into_owned();
        if screen_name.starts_with('.') {
            continue;
        }
        for page in std::fs::read_dir(screen.path()).unwrap().flatten() {
            let path = page.path();
            if path.extension().and_then(|e| e.to_str()) == Some("html") {
                let state = path.file_stem().unwrap().to_string_lossy().into_owned();
                out.push((screen_name.clone(), state));
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// Chrome, the gallery and the screenshots
// ---------------------------------------------------------------------------------------------

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn baselines_dir() -> PathBuf {
    manifest_dir().join("tests/visual/baselines")
}

/// The workspace `target/` folder (honours `CARGO_TARGET_DIR`).
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| manifest_dir().join("../../target"), PathBuf::from)
}

/// Find Chrome: `CAMERATA_CHROME`, then the macOS app, then well-known names on `PATH`.
fn find_chrome() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("CAMERATA_CHROME") {
        let p = PathBuf::from(p);
        return if p.exists() {
            Ok(p)
        } else {
            Err(format!(
                "CAMERATA_CHROME is set to {} which does not exist",
                p.display()
            ))
        };
    }
    let mac = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    if mac.exists() {
        return Ok(mac);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(
        "Google Chrome was not found (looked at CAMERATA_CHROME, the macOS app and PATH)"
            .to_string(),
    )
}

fn installed_chrome() -> (PathBuf, String) {
    let chrome = find_chrome().unwrap_or_else(|e| {
        panic!("visual regression needs Chrome and does not skip: {e}. Install Chrome (or set CAMERATA_CHROME) and, if the version changed, re-approve after review with scripts/visual-approve.sh")
    });
    let out = Command::new(&chrome)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", chrome.display()));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let version = parse_chrome_version(&text).unwrap_or_else(|| {
        panic!(
            "could not read a version from `{} --version`: {text:?}",
            chrome.display()
        )
    });
    (chrome, version)
}

/// Render the gallery with the real binary into a fresh `target/visual-run/<tag>` folder.
fn render_gallery(tag: &str) -> PathBuf {
    let dir = target_dir().join("visual-run").join(tag);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_camerata-ui"))
        .arg("gallery")
        .arg(&dir)
        .env("CAMERATA_NO_KEYCHAIN", "1")
        .stdout(Stdio::null())
        .status()
        .expect("could not run the camerata-ui binary");
    assert!(status.success(), "`camerata-ui gallery` failed: {status}");
    for (screen, state) in gallery_states(&dir) {
        let page = dir.join(&screen).join(format!("{state}.html"));
        let html = std::fs::read_to_string(&page).unwrap();
        std::fs::write(&page, prepare_page(&html, &screen)).unwrap();
    }
    // The "Show next 500" row of `tree/30k`, thousands of rows below the cap: its own short
    // capture, so that row and its neighbours are compared (see `CAPPED`).
    let long = dir.join("tree/30k.html");
    let bottom = scrolled_to_show_next(&std::fs::read_to_string(&long).unwrap());
    std::fs::write(dir.join(format!("tree/30k{SHOW_NEXT_SUFFIX}.html")), bottom).unwrap();
    dir
}

/// Run one Chrome launch to completion or kill it at the timeout.
fn run_chrome(
    chrome: &Path,
    profile: &Path,
    page: &Path,
    width: u32,
    window: u32,
    out: &Path,
) -> bool {
    let mut child = match Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--no-first-run",
            "--no-default-browser-check",
            // Pages must not depend on the network (see the module comment).
            "--host-resolver-rules=MAP * ~NOTFOUND",
            "--force-device-scale-factor=1",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--window-size={width},{window}"))
        .arg(format!("--screenshot={}", out.display()))
        .arg(format!("file://{}", page.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > CHROME_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    std::fs::metadata(out).is_ok_and(|m| m.len() > 0)
}

/// Screenshot `states` of a rendered gallery at every width into `shots`, a few Chromes at once.
/// Returns the stems that could not be captured.
/// The two window heights: the tall one most states are photographed in, and the `CAPTURE_HEIGHT`
/// one for the short captures (`is_short`: `shell/*`, whose gradient background is drawn across
/// the viewport, and the `-show-next` pages), which must be photographed in a viewport of the height
/// that is kept.
#[derive(Clone, Copy)]
struct Windows {
    tall: u32,
    shell: u32,
}

impl Windows {
    fn for_state(self, screen: &str, state: &str) -> u32 {
        if is_short(&format!("{screen}/{state}")) {
            self.shell
        } else {
            self.tall
        }
    }
}

fn screenshot_all(
    chrome: &Path,
    gallery: &Path,
    shots: &Path,
    states: &[(String, String)],
    windows: Windows,
) -> Vec<String> {
    std::fs::create_dir_all(shots).unwrap();
    let jobs: Vec<(String, String, u32)> = states
        .iter()
        .flat_map(|(s, st)| WIDTHS.iter().map(move |w| (s.clone(), st.clone(), *w)))
        .collect();
    let next = AtomicUsize::new(0);
    let failed = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for worker in 0..WORKERS.min(jobs.len().max(1)) {
            let (jobs, next, failed) = (&jobs, &next, &failed);
            scope.spawn(move || {
                let profile = gallery.join(format!(".chrome-profile-{worker}"));
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((screen, state, w)) = jobs.get(i) else {
                        break;
                    };
                    let page = gallery.join(screen).join(format!("{state}.html"));
                    let out = shots.join(format!("{}.png", stem(screen, state, *w)));
                    // One retry: a Chrome launch can fail once under load.
                    let ok = run_chrome(
                        chrome,
                        &profile,
                        &page,
                        *w,
                        windows.for_state(screen, state),
                        &out,
                    ) || run_chrome(
                        chrome,
                        &profile,
                        &page,
                        *w,
                        windows.for_state(screen, state),
                        &out,
                    );
                    if !ok {
                        failed.lock().unwrap().push(stem(screen, state, *w));
                    }
                }
            });
        }
    });
    failed.into_inner().unwrap()
}

/// Render and screenshot everything: the gallery folder, its states and the screenshot folder.
/// Screenshot the 100vh probe page at `width` in a window of `window` pixels and return how many
/// rows from the top it painted (the viewport height) and the screenshot's own height.
fn probe(chrome: &Path, gallery: &Path, width: u32, window: u32) -> (u32, u32) {
    let page = gallery.join("probe.html");
    std::fs::write(&page, PROBE_HTML).unwrap();
    let out = gallery.join(format!("probe-{width}-{window}.png"));
    let profile = gallery.join(".chrome-profile-probe");
    assert!(
        run_chrome(chrome, &profile, &page, width, window, &out),
        "Chrome could not screenshot the viewport probe page"
    );
    let img = decode(&std::fs::read(&out).unwrap()).unwrap();
    (painted_rows(&img), img.height())
}

/// Measure the window-to-viewport bar with the probe at both widths (in a window far taller than
/// the capture, so the whole viewport shows) and fail if it is not `EXPECTED_BAR` or differs by
/// width.
fn measure_bar(chrome: &Path, gallery: &Path) -> u32 {
    let tall = MAX_CAPTURE_HEIGHT + 600;
    let bars: Vec<u32> = WIDTHS
        .iter()
        .map(|w| bar_from_probe(tall, probe(chrome, gallery, *w, tall).0))
        .collect();
    assert!(
        bars.iter().all(|b| *b == EXPECTED_BAR),
        "Chrome's window-to-viewport bar is {bars:?} pixels at widths {WIDTHS:?}, not the \
         EXPECTED_BAR of {EXPECTED_BAR} the baselines were laid out with: re-measure, update \
         EXPECTED_BAR in review, then re-approve with scripts/visual-approve.sh"
    );
    bars[0]
}

fn capture(tag: &str) -> (Vec<(String, String)>, PathBuf, Vec<String>) {
    let (chrome, _) = installed_chrome();
    let gallery = render_gallery(tag);
    let bar = measure_bar(&chrome, &gallery);
    let windows = Windows {
        tall: window_height(MAX_CAPTURE_HEIGHT, bar),
        shell: window_height(CAPTURE_HEIGHT, bar),
    };
    // The proof the layout and the capture agree: in each window used, the probe's last painted
    // row is the window's last row, so the viewport is the whole screenshot and every row a state
    // paints (up to MAX_CAPTURE_HEIGHT; CAPTURE_HEIGHT for shell) is there to be kept and compared.
    for (window, rows) in [
        (windows.tall, MAX_CAPTURE_HEIGHT),
        (windows.shell, CAPTURE_HEIGHT),
    ] {
        for w in WIDTHS {
            let (painted, shot_height) = probe(&chrome, &gallery, w, window);
            assert_eq!(
                painted, rows,
                "at {w} wide a {window}-tall window paints {painted} rows, not the {rows} that are kept"
            );
            assert!(
                shot_height >= rows,
                "the probe screenshot is only {shot_height} rows tall"
            );
        }
    }
    let states = gallery_states(&gallery);
    let shots = gallery.join("shots");
    let failed = screenshot_all(&chrome, &gallery, &shots, &states, windows);
    (states, shots, failed)
}

fn assert_environment_matches_baselines() -> String {
    let (_, version) = installed_chrome();
    let recorded = std::fs::read_to_string(baselines_dir().join("CHROME_VERSION")).ok();
    if let Err(e) = check_environment(recorded.as_deref(), &version, &platform()) {
        panic!("visual regression cannot run: {e}");
    }
    version
}

// ---------------------------------------------------------------------------------------------
// The tests that drive Chrome
// ---------------------------------------------------------------------------------------------

#[test]
fn visual_regression_matches_baselines() {
    let started = Instant::now();
    assert_environment_matches_baselines();
    let (states, shots, capture_failed) = capture("check");
    eprintln!(
        "visual: rendered and screenshotted in {:?}",
        started.elapsed()
    );
    assert!(
        capture_failed.is_empty(),
        "Chrome could not screenshot: {capture_failed:?}"
    );
    let diff_dir = target_dir().join("visual-diff");
    if diff_dir.exists() {
        std::fs::remove_dir_all(&diff_dir).unwrap();
    }
    let failures = check_states(
        &baselines_dir(),
        &shots,
        &states,
        TOLERANCE,
        Some(&diff_dir),
    );
    if !failures.is_empty() {
        let mut msg = format!(
            "{} of {} screenshots differ from their approved baselines (review the diffs in {}, \
             then re-approve with scripts/visual-approve.sh [screen/state ...]):\n",
            failures.len(),
            states.len() * WIDTHS.len(),
            diff_dir.display()
        );
        for f in &failures {
            msg.push_str(&format!("  {}: {}\n", f.what, f.why));
        }
        panic!("{msg}");
    }
}

/// Re-approve baselines. Driven by `scripts/visual-approve.sh`, never by the default run.
/// `CAMERATA_VISUAL_APPROVE_STATES` is a space-separated list of `screen/state`; empty means all.
/// A baseline is rewritten only when it is missing or its image differs past the tolerance (or
/// the Chrome version changed, which re-approves everything), so a re-approval touches exactly
/// the files whose pixels changed; `CAMERATA_VISUAL_APPROVE_FORCE=1` rewrites every selected one.
#[test]
#[ignore = "re-approves baselines; run it through scripts/visual-approve.sh"]
fn visual_approve() {
    let (chrome, version) = installed_chrome();
    let _ = chrome;
    let baselines = baselines_dir();
    std::fs::create_dir_all(&baselines).unwrap();
    let wanted: Vec<String> = std::env::var("CAMERATA_VISUAL_APPROVE_STATES")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let (states, shots, capture_failed) = capture("approve");
    assert!(
        capture_failed.is_empty(),
        "Chrome could not screenshot: {capture_failed:?}"
    );
    for w in &wanted {
        assert!(
            states.iter().any(|(s, st)| format!("{s}/{st}") == *w),
            "unknown state {w:?}; states look like screen/state, for example tree/markers"
        );
    }
    let recorded = std::fs::read_to_string(baselines.join("CHROME_VERSION")).ok();
    let environment_changed =
        check_environment(recorded.as_deref(), &version, &platform()).is_err();
    let all = wanted.is_empty();
    // A state that runs past the cap without being named in CAPPED (or a named one that no
    // longer does) is not approved: its bottom would be compared from nowhere.
    let mut cap_failures = Vec::new();
    for (screen, state) in &states {
        let id = format!("{screen}/{state}");
        if !all && !wanted.contains(&id) {
            continue;
        }
        for w in WIDTHS {
            let name = stem(screen, state, w);
            let actual = std::fs::read(shots.join(format!("{name}.png"))).unwrap();
            let (_, reaches_cap) = compare_shot(&id, None, &actual, TOLERANCE);
            if let Some(why) = cap_failure(&id, reaches_cap) {
                cap_failures.push(format!("{id} @{w}: {why}"));
            }
        }
    }
    assert!(
        cap_failures.is_empty(),
        "nothing was approved:\n{}",
        cap_failures.join("\n")
    );
    let mut written = Vec::new();
    let mut unchanged = 0usize;
    for (screen, state) in &states {
        let id = format!("{screen}/{state}");
        if !all && !wanted.contains(&id) {
            continue;
        }
        for w in WIDTHS {
            let name = stem(screen, state, w);
            let actual = std::fs::read(shots.join(format!("{name}.png"))).unwrap();
            let target = baselines.join(format!("{name}.png"));
            let existing = std::fs::read(&target).ok();
            let differs = environment_changed
                || std::env::var_os("CAMERATA_VISUAL_APPROVE_FORCE").is_some()
                || compare(&id, existing.as_deref(), &actual, TOLERANCE) != Verdict::Match;
            if differs {
                std::fs::write(&target, normalize_screenshot(&id, &actual).unwrap()).unwrap();
                written.push(format!("{name}.png"));
            } else {
                unchanged += 1;
            }
        }
    }
    let mut removed = Vec::new();
    if all {
        let expected: BTreeSet<String> = states
            .iter()
            .flat_map(|(s, st)| WIDTHS.iter().map(move |w| stem(s, st, *w)))
            .collect();
        for stale in png_stems(&baselines).difference(&expected) {
            std::fs::remove_file(baselines.join(format!("{stale}.png"))).unwrap();
            removed.push(format!("{stale}.png"));
        }
    }
    std::fs::write(
        baselines.join("CHROME_VERSION"),
        version_file_content(&version),
    )
    .unwrap();
    println!("visual-approve: Chrome {version} on {}", platform());
    println!("visual-approve: {} baseline(s) written:", written.len());
    for f in &written {
        println!("  written  {f}");
    }
    for f in &removed {
        println!("  removed  {f} (its state no longer exists)");
    }
    println!("visual-approve: {unchanged} unchanged (within tolerance)");
}

/// Measure screenshot noise: render everything again and print, against the committed baselines,
/// how many pixels differ by more than each channel threshold. Used to choose `TOLERANCE`.
#[test]
#[ignore = "diagnostic: prints per-image render noise against the baselines"]
fn visual_measure_noise() {
    let (_, shots, capture_failed) = capture("measure");
    assert!(
        capture_failed.is_empty(),
        "Chrome could not screenshot: {capture_failed:?}"
    );
    let thresholds = [0u8, 1, 2, 3, 4, 6, 8, 12, 16, 32];
    let mut worst_delta = 0u8;
    let mut worst_pixels = vec![0usize; thresholds.len()];
    let mut images = 0usize;
    for stem in png_stems(&shots) {
        let a = decode(&std::fs::read(shots.join(format!("{stem}.png"))).unwrap()).unwrap();
        let Ok(bytes) = std::fs::read(baselines_dir().join(format!("{stem}.png"))) else {
            println!("noise: {stem}: no baseline");
            continue;
        };
        let b = decode(&bytes).unwrap();
        if a.dimensions() != b.dimensions() {
            println!(
                "noise: {stem}: SIZE DIFFERS {:?} vs {:?}",
                a.dimensions(),
                b.dimensions()
            );
            continue;
        }
        images += 1;
        let mut line = format!("noise: {stem}:");
        for (i, t) in thresholds.iter().enumerate() {
            let (max_delta, over) = pixel_stats(&a, &b, *t);
            worst_delta = worst_delta.max(max_delta);
            worst_pixels[i] = worst_pixels[i].max(over);
            if over > 0 {
                line.push_str(&format!(" >{t}:{over}"));
            }
        }
        println!("{line}");
    }
    println!("noise: {images} images compared");
    println!("noise: largest channel delta anywhere: {worst_delta}");
    for (t, p) in thresholds.iter().zip(&worst_pixels) {
        println!("noise: worst image has {p} pixels with a channel delta above {t}");
    }
}
