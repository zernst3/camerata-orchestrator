//! Look-and-feel: the shipped Camerata style kit a non-technical Product Owner
//! picks from at intake, captured into the onboarding document so the build
//! honors it.
//!
//! Per Zach (2026-06-14): the intake form ships ready-made Camerata color palettes
//! and style examples (button shape, font personality) the user can SELECT, plus
//! the ability to upload inspiration images the AI can interpret for styling cues.
//! All of it lands in the onboarding document ([`crate::form::IntakeForm::style`])
//! so the lead engineer and the build agents have an explicit, structured look to
//! honor instead of guessing from prose.
//!
//! These are consumer-facing CHOICES, not a theme engine: a small curated set that
//! looks good, named in plain language. The generated app's actual CSS is produced
//! downstream from the selected [`StylePreferences`]; this module is the typed
//! selection, not the renderer.

use serde::{Deserialize, Serialize};

// ─── shipped palettes ────────────────────────────────────────────────────────

/// One shipped color palette. Four roles cover a clean consumer app: the page
/// background, raised surfaces (cards), the ink (text), and a single accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    /// Stable id stored in the onboarding document (e.g. `"warm_studio"`).
    pub id: &'static str,
    /// Plain-language name shown in the picker.
    pub name: &'static str,
    /// One-line description of the mood, for the picker.
    pub description: &'static str,
    /// Page background hex.
    pub background: &'static str,
    /// Raised-surface (card) hex.
    pub surface: &'static str,
    /// Primary text (ink) hex.
    pub ink: &'static str,
    /// The single accent hex.
    pub accent: &'static str,
}

/// The palettes Camerata ships with. A curated handful, each tasteful on its own,
/// so a non-technical user cannot pick a bad-looking combination.
pub const SHIPPED_PALETTES: &[Palette] = &[
    Palette {
        id: "warm_studio",
        name: "Warm Studio",
        description: "Warm and handmade. Near-white paper, soft ink, a terracotta accent.",
        background: "#faf9f6",
        surface: "#ffffff",
        ink: "#1b1a18",
        accent: "#c8694a",
    },
    Palette {
        id: "clean_slate",
        name: "Clean Slate",
        description: "Crisp and professional. Cool greys with a confident blue accent.",
        background: "#f7f8fa",
        surface: "#ffffff",
        ink: "#1a1f2b",
        accent: "#2f6df0",
    },
    Palette {
        id: "forest",
        name: "Forest",
        description: "Calm and natural. Soft greens, a deep evergreen accent.",
        background: "#f6f8f5",
        surface: "#ffffff",
        ink: "#1c241c",
        accent: "#3f7a4f",
    },
    Palette {
        id: "midnight",
        name: "Midnight",
        description: "Dark and focused. Near-black surfaces with a luminous accent.",
        background: "#15171c",
        surface: "#1e2128",
        ink: "#eceef2",
        accent: "#6aa8ff",
    },
    Palette {
        id: "blossom",
        name: "Blossom",
        description: "Light and friendly. Warm off-white with a rosy accent.",
        background: "#fdf7f8",
        surface: "#ffffff",
        ink: "#2a1f24",
        accent: "#d4567f",
    },
];

impl Palette {
    /// Look up a shipped palette by id.
    pub fn by_id(id: &str) -> Option<&'static Palette> {
        SHIPPED_PALETTES.iter().find(|p| p.id == id)
    }
}

// ─── shape + font personality ────────────────────────────────────────────────

/// The shape language for buttons and surfaces, the most visible "style example"
/// a user reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ButtonStyle {
    /// Gently rounded corners (the friendly default).
    #[default]
    Rounded,
    /// Fully pill-shaped (soft, modern).
    Pill,
    /// Square, blocky corners (bold, utilitarian).
    Blocky,
}

impl ButtonStyle {
    /// A plain-language label for the picker.
    pub fn label(&self) -> &'static str {
        match self {
            ButtonStyle::Rounded => "Rounded",
            ButtonStyle::Pill => "Pill",
            ButtonStyle::Blocky => "Blocky",
        }
    }

    /// A representative CSS corner radius for the selected shape (consumed by the
    /// downstream renderer; here so the choice has a concrete meaning).
    pub fn radius_px(&self) -> u32 {
        match self {
            ButtonStyle::Rounded => 12,
            ButtonStyle::Pill => 999,
            ButtonStyle::Blocky => 0,
        }
    }
}

/// The font personality. Named by feel, not by family, so a non-technical user
/// picks a vibe; the renderer maps it to a concrete stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FontChoice {
    /// The clean native system font (the neutral default).
    #[default]
    System,
    /// A classic serif (editorial, trustworthy).
    Serif,
    /// A geometric sans (modern, friendly).
    Geometric,
    /// A monospace (technical, precise).
    Mono,
}

impl FontChoice {
    /// A plain-language label for the picker.
    pub fn label(&self) -> &'static str {
        match self {
            FontChoice::System => "System",
            FontChoice::Serif => "Serif",
            FontChoice::Geometric => "Geometric",
            FontChoice::Mono => "Monospace",
        }
    }
}

// ─── uploaded inspiration images ─────────────────────────────────────────────

/// A reference to an inspiration image the user uploaded for styling cues. Stored
/// as a path or content id plus an optional caption; the AI interprets the image
/// downstream (the bytes live on disk / in the store, not in the form).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    /// Path or content-id of the uploaded image.
    pub source: String,
    /// Optional one-line note from the user ("I love these warm tones").
    #[serde(default)]
    pub note: Option<String>,
}

impl ImageRef {
    /// Construct an image reference.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            note: None,
        }
    }

    /// Attach a note. Builder form.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

// ─── the selection ───────────────────────────────────────────────────────────

/// The PO's look-and-feel selections, captured into the onboarding document. All
/// fields are optional-by-default: an empty `StylePreferences` means "no strong
/// preference, lead engineer's choice," which is a valid, honest state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StylePreferences {
    /// The chosen shipped palette id, if any. `None` lets the engineer choose.
    #[serde(default)]
    pub palette_id: Option<String>,
    /// The chosen button/surface shape language.
    #[serde(default)]
    pub button_style: ButtonStyle,
    /// The chosen font personality.
    #[serde(default)]
    pub font: FontChoice,
    /// Inspiration images the user uploaded for the AI to interpret.
    #[serde(default)]
    pub reference_images: Vec<ImageRef>,
}

impl StylePreferences {
    /// Whether the user expressed any explicit look preference at all.
    pub fn is_specified(&self) -> bool {
        self.palette_id.is_some()
            || self.button_style != ButtonStyle::default()
            || self.font != FontChoice::default()
            || !self.reference_images.is_empty()
    }

    /// The resolved shipped [`Palette`], if a valid one was chosen.
    pub fn palette(&self) -> Option<&'static Palette> {
        self.palette_id.as_deref().and_then(Palette::by_id)
    }

    /// Render the selections as a plain-language block for the form brief, so the
    /// lead engineer and build agents see the chosen look. Returns `None` when no
    /// preference was expressed.
    pub fn render(&self) -> Option<String> {
        if !self.is_specified() {
            return None;
        }
        let mut out = String::from("Look and feel:\n");
        if let Some(p) = self.palette() {
            out.push_str(&format!("  Palette: {} ({})\n", p.name, p.description));
        } else if let Some(id) = &self.palette_id {
            out.push_str(&format!("  Palette: {id}\n"));
        }
        out.push_str(&format!("  Buttons: {}\n", self.button_style.label()));
        out.push_str(&format!("  Font: {}\n", self.font.label()));
        if !self.reference_images.is_empty() {
            out.push_str(&format!(
                "  Inspiration images ({}): interpret for styling cues\n",
                self.reference_images.len()
            ));
            for img in &self.reference_images {
                match &img.note {
                    Some(note) => out.push_str(&format!("    - {} — {note}\n", img.source)),
                    None => out.push_str(&format!("    - {}\n", img.source)),
                }
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ships_a_curated_set_of_palettes() {
        assert!(SHIPPED_PALETTES.len() >= 4);
        // Ids are unique.
        let mut ids: Vec<&str> = SHIPPED_PALETTES.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SHIPPED_PALETTES.len());
    }

    #[test]
    fn palette_lookup_by_id() {
        assert_eq!(Palette::by_id("warm_studio").unwrap().name, "Warm Studio");
        assert!(Palette::by_id("nope").is_none());
    }

    #[test]
    fn default_preferences_are_unspecified() {
        let prefs = StylePreferences::default();
        assert!(!prefs.is_specified());
        assert!(prefs.render().is_none());
        assert_eq!(prefs.button_style, ButtonStyle::Rounded);
        assert_eq!(prefs.font, FontChoice::System);
    }

    #[test]
    fn specified_preferences_render_a_brief_block() {
        let prefs = StylePreferences {
            palette_id: Some("forest".into()),
            button_style: ButtonStyle::Blocky,
            font: FontChoice::Serif,
            reference_images: vec![ImageRef::new("mood1.png").with_note("love these greens")],
        };
        assert!(prefs.is_specified());
        let rendered = prefs.render().unwrap();
        assert!(rendered.contains("Forest"));
        assert!(rendered.contains("Blocky"));
        assert!(rendered.contains("Serif"));
        assert!(rendered.contains("mood1.png"));
        assert!(rendered.contains("love these greens"));
    }

    #[test]
    fn button_style_has_concrete_radius() {
        assert_eq!(ButtonStyle::Pill.radius_px(), 999);
        assert_eq!(ButtonStyle::Blocky.radius_px(), 0);
    }

    #[test]
    fn preferences_round_trip_json() {
        let prefs = StylePreferences {
            palette_id: Some("midnight".into()),
            button_style: ButtonStyle::Pill,
            font: FontChoice::Geometric,
            reference_images: vec![ImageRef::new("a.png")],
        };
        let json = serde_json::to_string(&prefs).unwrap();
        let back: StylePreferences = serde_json::from_str(&json).unwrap();
        assert_eq!(back, prefs);
    }

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&ButtonStyle::Blocky).unwrap(),
            "\"blocky\""
        );
        assert_eq!(
            serde_json::to_string(&FontChoice::Geometric).unwrap(),
            "\"geometric\""
        );
    }
}
