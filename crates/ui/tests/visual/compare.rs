//! The pure half of the visual regression: comparing PNGs, the baseline folder checks and the
//! page preparation. No Chrome in here, so the unit tests in `visual_compare.rs` run in every
//! default `cargo test`; `visual.rs` (behind the `visual-baselines` feature) drives Chrome with it.

use std::collections::BTreeSet;
use std::path::Path;

use image::RgbaImage;

/// The two widths of spec U6. No third.
pub const WIDTHS: [u32; 2] = [1440, 1000];

/// The probe page's fill colour (see `probe_html`).
pub const PROBE_COLOUR: [u8; 4] = [255, 0, 0, 255];

/// A page whose only content is a `100vh` block of `PROBE_COLOUR`: in a screenshot, the painted
/// rows are exactly the viewport, and the rows below them (white) are the window-to-viewport bar.
pub const PROBE_HTML: &str = "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
    html, body { margin: 0; padding: 0; } \
    div { height: 100vh; background: rgb(255, 0, 0); }</style></head><body><div></div></body></html>";

/// How many rows from the top of a probe screenshot are `PROBE_COLOUR`: the viewport height.
pub fn painted_rows(img: &RgbaImage) -> u32 {
    (0..img.height())
        .take_while(|y| img.get_pixel(0, *y).0 == PROBE_COLOUR)
        .count() as u32
}

/// The window-to-viewport bar: what a window of `window` pixels loses to browser chrome, given
/// the viewport height the probe painted.
pub fn bar_from_probe(window: u32, painted: u32) -> u32 {
    window.saturating_sub(painted)
}

/// The window height whose viewport is exactly `capture` rows (a window is `bar` taller than its
/// viewport). The page is then laid out for the captured height, so every painted row is
/// compared and none hides below the crop.
pub fn window_height(capture: u32, bar: u32) -> u32 {
    capture + bar
}

/// The least height of a stored screenshot, and the whole height of every `shell/*` one (the shell
/// frame is short, and its gradient background depends on the window, so it is cut here).
pub const CAPTURE_HEIGHT: u32 = 1400;

/// The most a screenshot keeps. Every other state is stored as tall as its painted content
/// (`kept_height`), at least `CAPTURE_HEIGHT` and at most this, so a change anywhere in a page
/// shorter than this is compared. Measured 2026-11-05: every state but one paints at most 3868
/// rows (`queue/30k` at 1000); `tree/30k` paints at least 12,000 (a long generated list of
/// near-identical rows) and keeps its first 4800. The cap is never silent: a state whose content
/// reaches it fails unless it is named in `CAPPED`.
pub const MAX_CAPTURE_HEIGHT: u32 = 4800;

/// A page whose painted content ends within this many rows of `MAX_CAPTURE_HEIGHT` counts as
/// reaching the cap: a list cut by the window ends on a row boundary, not exactly on the last
/// row (measured: `tree/30k` at 1440 ends 3 rows short), and a state this close has no headroom.
pub const CAP_MARGIN: u32 = 64;

/// The states that are expected to run past `MAX_CAPTURE_HEIGHT`, so only their first rows are
/// compared. `tree/30k` is a 30,000-row generated list, thousands of rows taller than any cap
/// worth a baseline; its end (the "Show next 500" row) is compared by the `tree/30k-show-next`
/// capture instead. Any other state reaching the cap, or a named one that stops reaching it,
/// fails (`cap_failure`): a screen can no longer outgrow its baseline unseen.
pub const CAPPED: &[&str] = &["tree/30k"];

/// The suffix of a state whose page is `scrolled_to_show_next`: one `CAPTURE_HEIGHT` screenful of
/// the page it is named after, around its "Show next" row.
pub const SHOW_NEXT_SUFFIX: &str = "-show-next";

/// States whose page is a full-window overlay (the code viewer is sized to the viewport), so a
/// taller window only makes the overlay taller: the first WO16c re-approval found
/// `detail/viewer-commit` "reaching the cap" in the 4800 window for that reason, not content.
pub const VIEWPORT_SIZED: &[&str] = &["detail/viewer-commit"];

/// Whether the capture of state `id` (`screen/state`) is the short kind: `CAPTURE_HEIGHT` tall, in
/// a window that tall. That is every `shell/*` state (whose gradient background is drawn across
/// the viewport), every `-show-next` capture (one screenful of a long page) and every
/// `VIEWPORT_SIZED` overlay.
pub fn is_short(id: &str) -> bool {
    id.starts_with("shell/") || id.ends_with(SHOW_NEXT_SUFFIX) || VIEWPORT_SIZED.contains(&id)
}

/// The failure, if any, for state `id` whose actual screenshot's content does or does not
/// `reaches_cap` (`MAX_CAPTURE_HEIGHT`). Short captures are not judged.
pub fn cap_failure(id: &str, reaches_cap: bool) -> Option<String> {
    if is_short(id) {
        return None;
    }
    let named = CAPPED.contains(&id);
    if reaches_cap && !named {
        Some(format!(
            "{id} grows past the {MAX_CAPTURE_HEIGHT}-row cap, so its bottom is no longer \
             compared: raise MAX_CAPTURE_HEIGHT in review, or name it in CAPPED if it is a \
             list that is meant to be cut"
        ))
    } else if !reaches_cap && named {
        Some(format!(
            "{id} is named in CAPPED but no longer reaches the {MAX_CAPTURE_HEIGHT}-row cap: \
             remove it from CAPPED"
        ))
    } else {
        None
    }
}

/// The flat page background the test paints off the shell screen (see `FLAT_BACKGROUND_STYLE`),
/// the colour whose absence marks a row as content.
pub const FLAT_BG: [u8; 3] = [0x12, 0x11, 0x10];

/// Rows from the top up to and including the last one holding any pixel that is not `FLAT_BG`.
pub fn content_height(img: &RgbaImage) -> u32 {
    (0..img.height())
        .rev()
        .find(|y| (0..img.width()).any(|x| img.get_pixel(x, *y).0[..3] != FLAT_BG))
        .map_or(0, |y| y + 1)
}

/// How many rows of a screenshot of state `id` are stored and compared: `CAPTURE_HEIGHT` for a
/// short capture (`is_short`), otherwise the painted content height clamped to
/// `CAPTURE_HEIGHT..=MAX_CAPTURE_HEIGHT`.
pub fn kept_height(img: &RgbaImage, id: &str) -> u32 {
    if is_short(id) {
        CAPTURE_HEIGHT
    } else {
        content_height(img).clamp(CAPTURE_HEIGHT, MAX_CAPTURE_HEIGHT)
    }
}

/// What counts as a visual change. A pixel is "changed" when any of its four channels differs by
/// more than `max_channel_delta`; an image matches when it has at most `max_changed_pixels`
/// changed pixels and the same size as its baseline.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    pub max_channel_delta: u8,
    pub max_changed_pixels: usize,
}

/// How one screenshot compares with its approved baseline.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
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

pub fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map(|i| i.to_rgba8())
        .map_err(|e| e.to_string())
}

/// A decoded Chrome screenshot: the rows that are compared, and whether its painted content
/// comes within `CAP_MARGIN` of `MAX_CAPTURE_HEIGHT` (so it was cut there).
pub struct Shot {
    pub kept: RgbaImage,
    pub reaches_cap: bool,
}

/// Decode a Chrome screenshot of state `id` and keep its top `kept_height` rows. A screenshot
/// already that size or shorter is kept as it is, so a short one still reads as a size change.
pub fn decode_shot(id: &str, bytes: &[u8]) -> Result<Shot, String> {
    let img = decode(bytes)?;
    let keep = kept_height(&img, id);
    let reaches_cap = !is_short(id) && content_height(&img) > MAX_CAPTURE_HEIGHT - CAP_MARGIN;
    let kept = if img.height() > keep {
        image::imageops::crop_imm(&img, 0, 0, img.width(), keep).to_image()
    } else {
        img
    };
    Ok(Shot { kept, reaches_cap })
}

pub fn decode_actual(id: &str, bytes: &[u8]) -> Result<RgbaImage, String> {
    decode_shot(id, bytes).map(|s| s.kept)
}

/// Per-pixel statistics of two same-size images: the largest channel delta anywhere, and how many
/// pixels have a channel delta above `threshold`.
pub fn pixel_stats(a: &RgbaImage, b: &RgbaImage, threshold: u8) -> (u8, usize) {
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

/// Compare an actual screenshot of state `id` with its baseline (`None` when no baseline exists).
pub fn compare(id: &str, baseline: Option<&[u8]>, actual: &[u8], tol: Tolerance) -> Verdict {
    compare_shot(id, baseline, actual, tol).0
}

/// `compare`, and whether the actual's content reaches `MAX_CAPTURE_HEIGHT` (judged by
/// `cap_failure`), decoding the screenshot once.
pub fn compare_shot(
    id: &str,
    baseline: Option<&[u8]>,
    actual: &[u8],
    tol: Tolerance,
) -> (Verdict, bool) {
    let shot = match decode_shot(id, actual) {
        Ok(s) => s,
        Err(e) => return (Verdict::Undecodable(format!("actual: {e}")), false),
    };
    (compare_decoded(baseline, &shot.kept, tol), shot.reaches_cap)
}

fn compare_decoded(baseline: Option<&[u8]>, act: &RgbaImage, tol: Tolerance) -> Verdict {
    let Some(baseline) = baseline else {
        return Verdict::MissingBaseline;
    };
    let base = match decode(baseline) {
        Ok(i) => i,
        Err(e) => return Verdict::Undecodable(format!("baseline: {e}")),
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
    let (max_delta, changed_pixels) = pixel_stats(&base, act, tol.max_channel_delta);
    if changed_pixels <= tol.max_changed_pixels {
        Verdict::Match
    } else {
        Verdict::Changed {
            changed_pixels,
            max_delta,
        }
    }
}

/// The top `kept_height` rows of a screenshot of state `id`, re-encoded as an RGB PNG (lossless: no
/// pixel in the kept rows changes). Used to store a baseline; a comparison crops with
/// `decode_actual` and never re-encodes. A screenshot shorter than `CAPTURE_HEIGHT` is refused.
pub fn normalize_screenshot(id: &str, png: &[u8]) -> Result<Vec<u8>, String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let rgba = decode(png)?;
    if rgba.height() < CAPTURE_HEIGHT {
        return Err(format!(
            "Chrome's screenshot is only {} pixels tall, need at least {CAPTURE_HEIGHT}",
            rgba.height()
        ));
    }
    let keep = kept_height(&rgba, id);
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    let kept = image::imageops::crop_imm(&rgb, 0, 0, rgb.width(), keep).to_image();
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
pub fn diff_image(baseline: &RgbaImage, actual: &RgbaImage, tol: Tolerance) -> Option<RgbaImage> {
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
pub struct Failure {
    /// `screen/state @width`, or the baseline file name for a stale baseline.
    pub what: String,
    pub why: String,
}

/// `<screen>-<state>-<width>` for a gallery state at a width: the baseline file stem.
pub fn stem(screen: &str, state: &str, width: u32) -> String {
    format!("{screen}-{state}-{width}")
}

/// The `.png` file stems in a folder (empty when the folder is missing).
pub fn png_stems(dir: &Path) -> BTreeSet<String> {
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
pub fn check_states(
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
            let id = format!("{screen}/{state}");
            let (verdict, reaches_cap) = compare_shot(&id, baseline.as_deref(), &actual, tol);
            if let Some(why) = cap_failure(&id, reaches_cap) {
                failures.push(Failure {
                    what: what.clone(),
                    why,
                });
            }
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
                if let (Some(b), Ok(a)) =
                    (baseline.as_deref().map(decode), decode_actual(&id, &actual))
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

/// `156.0.8078.80` from `Google Chrome 156.0.8078.80 `.
pub fn parse_chrome_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

pub fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// The platform half of `CHROME_VERSION`: fonts and anti-aliasing differ per OS and CPU.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The content `scripts/visual-approve.sh` writes to `baselines/CHROME_VERSION`.
pub fn version_file_content(version: &str) -> String {
    format!("{version}\n{}\n", platform())
}

/// Check the installed Chrome against `baselines/CHROME_VERSION`: same major version and same
/// platform. `recorded` is the file's content, `None` when it is missing.
pub fn check_environment(
    recorded: Option<&str>,
    version: &str,
    platform: &str,
) -> Result<(), String> {
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

/// Added to every page's `<head>` before it is screenshotted. The loading states animate a
/// skeleton shimmer, so two renders of the same page differ by timing (measured: up to 124,000
/// pixels and a channel delta of 8 in `engine/loading`, `queue/loading` and `tree/loading`). With
/// animations and transitions off, a page draws its first frame, the same every time. This
/// changes the copy under `target/` only, never a screen or the CSS.
pub const FREEZE_STYLE: &str = "<style>*, *::before, *::after { animation: none !important; \
    transition: none !important; caret-color: transparent !important; }</style>";

/// Added to every page except the `shell` screen's. The app's background is a dithered radial
/// gradient, and Chrome writes a dithered gradient as noise a PNG cannot compress: it made the
/// 112 baselines 59 MB, in a public repository that grows by every re-approval. A flat colour
/// compresses to almost nothing. The eight `shell/*` images (four states at two widths) keep the real background, so a change
/// to the background itself is still caught there (the shell frame is on every screen). The
/// colour is the gradient's edge colour; it changes the copy under `target/` only.
pub const FLAT_BACKGROUND_STYLE: &str = "<style>html, body { background: #121110 !important; \
    background-image: none !important; }</style>";

/// The page the test screenshots: `html` with `FREEZE_STYLE` (and, off the `shell` screen,
/// `FLAT_BACKGROUND_STYLE`) added just before `</head>` (at the start when there is none).
pub fn prepare_page(html: &str, screen: &str) -> String {
    let mut style = FREEZE_STYLE.to_string();
    if screen != "shell" {
        style.push_str(FLAT_BACKGROUND_STYLE);
    }
    match html.find("</head>") {
        Some(i) => format!("{}{style}{}", &html[..i], &html[i..]),
        None => format!("{style}{html}"),
    }
}

/// The page of a `-show-next` capture: `html` slid up so that its first "Show next" button (the
/// row under a list that loads more, which sits thousands of rows down `tree/30k`) is 700 rows
/// from the top of one `CAPTURE_HEIGHT` screenful, with the rows around it. The document is cut to
/// the viewport and its body translated, because Chrome's `--screenshot` cannot scroll. A page
/// with no such button slides to its end instead.
pub fn scrolled_to_show_next(html: &str) -> String {
    let style = format!(
        "<style>html {{ overflow: hidden !important; height: {CAPTURE_HEIGHT}px !important; }}</style>"
    );
    let script = format!(
        "<script>var h = document.body.scrollHeight; var shift = h - {CAPTURE_HEIGHT}; \
         document.querySelectorAll('button').forEach(function (b) {{ \
         if (/Show next/.test(b.textContent)) {{ shift = b.getBoundingClientRect().top - 700; }} }}); \
         document.body.style.transform = 'translateY(' + (-Math.max(0, shift)) + 'px)';</script>"
    );
    let with_style = match html.find("</head>") {
        Some(i) => format!("{}{style}{}", &html[..i], &html[i..]),
        None => format!("{style}{html}"),
    };
    match with_style.rfind("</body>") {
        Some(i) => format!("{}{script}{}", &with_style[..i], &with_style[i..]),
        None => format!("{with_style}{script}"),
    }
}
