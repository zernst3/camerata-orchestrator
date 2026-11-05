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
//! After a reviewed change, re-approve with `scripts/visual-approve.sh [screen/state ...]`.
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

use image::RgbaImage;

/// The two widths of spec U6. No third.
const WIDTHS: [u32; 2] = [1440, 1000];

/// The height of every stored screenshot. A state taller than this is cut here, in the baseline
/// and the actual alike.
const CAPTURE_HEIGHT: u32 = 1400;

/// The Chrome window height. Headless Chrome's viewport is about 130 pixels shorter than its
/// window, and content below the viewport is painted only sometimes (a measured flake: the
/// bottom 130 rows of `queue/line-lowered` were white in one render and content in another). The
/// window is made tall enough that the whole `CAPTURE_HEIGHT` is inside the viewport, and
/// `normalize_screenshot` crops to it.
const WINDOW_HEIGHT: u32 = CAPTURE_HEIGHT + 300;

/// Chrome processes run at once. The machine also builds Rust, so a few, not all cores.
const WORKERS: usize = 4;

/// One Chrome launch must finish inside this.
const CHROME_TIMEOUT: Duration = Duration::from_secs(90);

/// What counts as a visual change. A pixel is "changed" when any of its four channels differs by
/// more than `max_channel_delta`; an image matches when it has at most `max_changed_pixels`
/// changed pixels and the same size as its baseline.
#[derive(Clone, Copy, Debug)]
struct Tolerance {
    max_channel_delta: u8,
    max_changed_pixels: usize,
}

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

/// How one screenshot compares with its approved baseline.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Match,
    /// Same size, more changed pixels than the tolerance allows.
    Changed {
        changed_pixels: usize,
        max_delta: u8,
    },
    SizeChanged {
        baseline: (u32, u32),
        actual: (u32, u32),
    },
    MissingBaseline,
    Undecodable(String),
}

fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map(|i| i.to_rgba8())
        .map_err(|e| e.to_string())
}

/// Decode a Chrome screenshot and keep its top `CAPTURE_HEIGHT` rows (see `WINDOW_HEIGHT`). A
/// screenshot already that size or shorter is returned as it is, so a short one still reads as a
/// size change.
fn decode_actual(bytes: &[u8]) -> Result<RgbaImage, String> {
    let img = decode(bytes)?;
    if img.height() > CAPTURE_HEIGHT {
        Ok(image::imageops::crop_imm(&img, 0, 0, img.width(), CAPTURE_HEIGHT).to_image())
    } else {
        Ok(img)
    }
}

/// Per-pixel statistics of two same-size images: the largest channel delta anywhere, and how many
/// pixels have a channel delta above `threshold`.
fn pixel_stats(a: &RgbaImage, b: &RgbaImage, threshold: u8) -> (u8, usize) {
    let mut max_delta = 0u8;
    let mut over = 0usize;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        let delta =
            pa.0.iter()
                .zip(pb.0.iter())
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0);
        max_delta = max_delta.max(delta);
        if delta > threshold {
            over += 1;
        }
    }
    (max_delta, over)
}

/// Compare an actual screenshot with its baseline (`None` when no baseline exists).
fn compare(baseline: Option<&[u8]>, actual: &[u8], tol: Tolerance) -> Verdict {
    let Some(baseline) = baseline else {
        return Verdict::MissingBaseline;
    };
    let base = match decode(baseline) {
        Ok(i) => i,
        Err(e) => return Verdict::Undecodable(format!("baseline: {e}")),
    };
    let act = match decode_actual(actual) {
        Ok(i) => i,
        Err(e) => return Verdict::Undecodable(format!("actual: {e}")),
    };
    if base.dimensions() != act.dimensions() {
        return Verdict::SizeChanged {
            baseline: base.dimensions(),
            actual: act.dimensions(),
        };
    }
    if base.as_raw() == act.as_raw() {
        return Verdict::Match;
    }
    let (max_delta, changed_pixels) = pixel_stats(&base, &act, tol.max_channel_delta);
    if changed_pixels <= tol.max_changed_pixels {
        Verdict::Match
    } else {
        Verdict::Changed {
            changed_pixels,
            max_delta,
        }
    }
}

/// The top `CAPTURE_HEIGHT` rows of a screenshot, re-encoded as an RGB PNG (lossless: no pixel
/// in the kept rows changes). Chrome's window is taller than `CAPTURE_HEIGHT` (see
/// `WINDOW_HEIGHT`) so the whole kept area is inside its viewport, where painting is
/// deterministic; the rows below it are dropped. Used to store a baseline; a comparison crops
/// with `decode_actual` and never re-encodes.
fn normalize_screenshot(png: &[u8]) -> Result<Vec<u8>, String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let rgb = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?
        .to_rgb8();
    if rgb.height() < CAPTURE_HEIGHT {
        return Err(format!(
            "Chrome's screenshot is only {} pixels tall, need at least {CAPTURE_HEIGHT}",
            rgb.height()
        ));
    }
    let kept = image::imageops::crop_imm(&rgb, 0, 0, rgb.width(), CAPTURE_HEIGHT).to_image();
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Default, FilterType::Adaptive)
        .write_image(
            kept.as_raw(),
            kept.width(),
            kept.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// The actual image with every changed pixel (past the channel tolerance) painted magenta and the
/// rest dimmed, so the change stands out.
fn diff_image(baseline: &RgbaImage, actual: &RgbaImage, tol: Tolerance) -> Option<RgbaImage> {
    if baseline.dimensions() != actual.dimensions() {
        return None;
    }
    let mut out = actual.clone();
    for (x, y, p) in out.enumerate_pixels_mut() {
        let b = baseline.get_pixel(x, y);
        let changed =
            p.0.iter()
                .zip(b.0.iter())
                .any(|(u, v)| u.abs_diff(*v) > tol.max_channel_delta);
        *p = if changed {
            image::Rgba([255, 0, 255, 255])
        } else {
            let [r, g, bl, _] = p.0;
            image::Rgba([r / 3 + 170, g / 3 + 170, bl / 3 + 170, 255])
        };
    }
    Some(out)
}

/// One failing image.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    /// `screen/state @width`, or the baseline file name for a stale baseline.
    what: String,
    why: String,
}

/// `<screen>-<state>-<width>` for a gallery state at a width: the baseline file stem.
fn stem(screen: &str, state: &str, width: u32) -> String {
    format!("{screen}-{state}-{width}")
}

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

/// The `.png` file stems in a folder (empty when the folder is missing).
fn png_stems(dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("png") {
                out.insert(p.file_stem().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out
}

/// Compare every actual screenshot in `actual_dir` with `baselines`, and flag baselines whose
/// state no longer exists. When `diff_dir` is set, a changed image also writes its actual and its
/// diff there. `states` is every `(screen, state)` the gallery rendered.
fn check_states(
    baselines: &Path,
    actual_dir: &Path,
    states: &[(String, String)],
    tol: Tolerance,
    diff_dir: Option<&Path>,
) -> Vec<Failure> {
    let mut failures = Vec::new();
    let mut expected = BTreeSet::new();
    for (screen, state) in states {
        for w in WIDTHS {
            let name = stem(screen, state, w);
            expected.insert(name.clone());
            let what = format!("{screen}/{state} @{w}");
            let actual_path = actual_dir.join(format!("{name}.png"));
            let Ok(actual) = std::fs::read(&actual_path) else {
                failures.push(Failure {
                    what,
                    why: "Chrome produced no screenshot for this state".to_string(),
                });
                continue;
            };
            let baseline = std::fs::read(baselines.join(format!("{name}.png"))).ok();
            let verdict = compare(baseline.as_deref(), &actual, tol);
            let why = match &verdict {
                Verdict::Match => continue,
                Verdict::MissingBaseline => {
                    "no baseline: new state, run scripts/visual-approve.sh after review".to_string()
                }
                Verdict::Changed {
                    changed_pixels,
                    max_delta,
                } => format!(
                    "{changed_pixels} pixels changed (largest channel delta {max_delta}; \
                     allowed: {} pixels past delta {})",
                    tol.max_changed_pixels, tol.max_channel_delta
                ),
                Verdict::SizeChanged { baseline, actual } => format!(
                    "image size changed from {}x{} to {}x{}",
                    baseline.0, baseline.1, actual.0, actual.1
                ),
                Verdict::Undecodable(e) => format!("cannot decode a PNG ({e})"),
            };
            let mut why = why;
            if let Some(dir) = diff_dir {
                std::fs::create_dir_all(dir).unwrap();
                let actual_out = dir.join(format!("{name}-actual.png"));
                std::fs::write(&actual_out, &actual).unwrap();
                why.push_str(&format!("; actual: {}", actual_out.display()));
                if let (Some(b), Ok(a)) = (baseline.as_deref().map(decode), decode_actual(&actual))
                {
                    if let Some(diff) = b.ok().and_then(|b| diff_image(&b, &a, tol)) {
                        let diff_out = dir.join(format!("{name}-diff.png"));
                        diff.save(&diff_out).unwrap();
                        why.push_str(&format!("; diff: {}", diff_out.display()));
                    }
                }
            }
            failures.push(Failure { what, why });
        }
    }
    for stale in png_stems(baselines).difference(&expected) {
        failures.push(Failure {
            what: format!("{stale}.png"),
            why: "stale baseline: its state no longer exists, run scripts/visual-approve.sh \
                  after review"
                .to_string(),
        });
    }
    failures
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

/// `156.0.8078.80` from `Google Chrome 156.0.8078.80 `.
fn parse_chrome_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// The platform half of `CHROME_VERSION`: fonts and anti-aliasing differ per OS and CPU.
fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The content `scripts/visual-approve.sh` writes to `baselines/CHROME_VERSION`.
fn version_file_content(version: &str) -> String {
    format!("{version}\n{}\n", platform())
}

/// Check the installed Chrome against `baselines/CHROME_VERSION`: same major version and same
/// platform. `recorded` is the file's content, `None` when it is missing.
fn check_environment(recorded: Option<&str>, version: &str, platform: &str) -> Result<(), String> {
    let fix = "re-approve after review with scripts/visual-approve.sh";
    let Some(recorded) = recorded else {
        return Err(format!(
            "baselines/CHROME_VERSION is missing; installed Chrome is {version} on {platform}: \
             {fix}"
        ));
    };
    let mut lines = recorded.lines();
    let rec_version = lines.next().unwrap_or("").trim();
    let rec_platform = lines.next().unwrap_or("").trim();
    if major(rec_version) != major(version) {
        return Err(format!(
            "Chrome major version differs: baselines were approved with {rec_version}, this \
             machine has {version}: {fix}"
        ));
    }
    if rec_platform != platform {
        return Err(format!(
            "platform differs: baselines were approved on {rec_platform}, this machine is \
             {platform}: {fix}"
        ));
    }
    Ok(())
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

/// Added to every page's `<head>` before it is screenshotted. The loading states animate a
/// skeleton shimmer, so two renders of the same page differ by timing (measured: up to 124,000
/// pixels and a channel delta of 8 in `engine/loading`, `queue/loading` and `tree/loading`). With
/// animations and transitions off, a page draws its first frame, the same every time. This
/// changes the copy under `target/` only, never a screen or the CSS.
const FREEZE_STYLE: &str = "<style>*, *::before, *::after { animation: none !important; \
    transition: none !important; caret-color: transparent !important; }</style>";

/// `html` with `FREEZE_STYLE` added just before `</head>` (at the start when there is none).
fn freeze_animations(html: &str) -> String {
    match html.find("</head>") {
        Some(i) => format!("{}{FREEZE_STYLE}{}", &html[..i], &html[i..]),
        None => format!("{FREEZE_STYLE}{html}"),
    }
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
        std::fs::write(&page, freeze_animations(&html)).unwrap();
    }
    dir
}

/// Run one Chrome launch to completion or kill it at the timeout.
fn run_chrome(chrome: &Path, profile: &Path, page: &Path, width: u32, out: &Path) -> bool {
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
        .arg(format!("--window-size={width},{WINDOW_HEIGHT}"))
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
fn screenshot_all(
    chrome: &Path,
    gallery: &Path,
    shots: &Path,
    states: &[(String, String)],
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
                    let ok = run_chrome(chrome, &profile, &page, *w, &out)
                        || run_chrome(chrome, &profile, &page, *w, &out);
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
fn capture(tag: &str) -> (Vec<(String, String)>, PathBuf, Vec<String>) {
    let (chrome, _) = installed_chrome();
    let gallery = render_gallery(tag);
    let states = gallery_states(&gallery);
    let shots = gallery.join("shots");
    let failed = screenshot_all(&chrome, &gallery, &shots, &states);
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
    let mut written = Vec::new();
    let mut unchanged = 0usize;
    for (screen, state) in &states {
        if !all && !wanted.contains(&format!("{screen}/{state}")) {
            continue;
        }
        for w in WIDTHS {
            let name = stem(screen, state, w);
            let actual = std::fs::read(shots.join(format!("{name}.png"))).unwrap();
            let target = baselines.join(format!("{name}.png"));
            let existing = std::fs::read(&target).ok();
            let differs = environment_changed
                || std::env::var_os("CAMERATA_VISUAL_APPROVE_FORCE").is_some()
                || compare(existing.as_deref(), &actual, TOLERANCE) != Verdict::Match;
            if differs {
                std::fs::write(&target, normalize_screenshot(&actual).unwrap()).unwrap();
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

#[test]
fn freezing_animations_adds_one_style_block_inside_the_head() {
    let frozen = freeze_animations("<html><head><title>t</title></head><body>x</body></html>");
    assert_eq!(frozen.matches("animation: none").count(), 1);
    assert!(frozen.find("animation: none").unwrap() < frozen.find("</head>").unwrap());
    assert!(freeze_animations("<p>no head</p>").starts_with("<style>"));
}
