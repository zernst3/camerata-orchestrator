//! The pure half of the visual regression: comparing PNGs, the baseline folder checks and the
//! page preparation. No Chrome in here, so the unit tests in `visual_compare.rs` run in every
//! default `cargo test`; `visual.rs` (behind the `visual-baselines` feature) drives Chrome with it.

use std::collections::BTreeSet;
use std::path::Path;

use image::RgbaImage;

/// The two widths of spec U6. No third.
pub const WIDTHS: [u32; 2] = [1440, 1000];

/// The height of every stored screenshot. A state taller than this is cut here, in the baseline
/// and the actual alike.
pub const CAPTURE_HEIGHT: u32 = 1400;

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

/// Decode a Chrome screenshot and keep its top `CAPTURE_HEIGHT` rows (see `WINDOW_HEIGHT`). A
/// screenshot already that size or shorter is returned as it is, so a short one still reads as a
/// size change.
pub fn decode_actual(bytes: &[u8]) -> Result<RgbaImage, String> {
    let img = decode(bytes)?;
    if img.height() > CAPTURE_HEIGHT {
        Ok(image::imageops::crop_imm(&img, 0, 0, img.width(), CAPTURE_HEIGHT).to_image())
    } else {
        Ok(img)
    }
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

/// Compare an actual screenshot with its baseline (`None` when no baseline exists).
pub fn compare(baseline: Option<&[u8]>, actual: &[u8], tol: Tolerance) -> Verdict {
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
pub fn normalize_screenshot(png: &[u8]) -> Result<Vec<u8>, String> {
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
/// compresses to almost nothing. The six `shell/*` images keep the real background, so a change
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
