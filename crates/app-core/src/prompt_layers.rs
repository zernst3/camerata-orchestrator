//! The geological prompt layering (prefix-cache-optimal assembly).
//!
//! Claude, DeepSeek, and GLM all do automatic PREFIX caching: overlapping text at the START
//! of a prompt is billed at a deep discount, and the cache breaks the moment a token differs
//! from the prior request. Camerata OWNS prompt assembly, so it can GUARANTEE a cache-optimal
//! structure mechanically. Every agent prompt is assembled in one fixed order, static at the
//! TOP and volatile at the BOTTOM, so the stable prefix stays byte-identical across calls that
//! differ only in the volatile tail (the story being worked, the latest toolchain/gate error,
//! the diff/turn request):
//!
//! - **Layer 1 (global immutable):** identical across ALL calls for a project. The governance
//!   kernel + role framing + project global rules/schemas. Maximal cache.
//! - **Layer 2 (epic/session context):** changes every few days. The grounding block (repo
//!   digest + rule context) for the current work area. Highly cached.
//! - **Layer 3 (volatile execution state):** the exact story, THE LATEST toolchain/gate error
//!   (the LIFECYCLE-5 feedback), the diff/turn request. Never cached.
//!
//! [`LayeredPrompt`] is a pure, deterministic assembler: given the same Layer-1 and Layer-2
//! text it serializes a byte-identical stable prefix regardless of the Layer-3 tail, and it
//! exposes [`LayeredPrompt::stable_prefix_len`] (the byte length of Layers 1+2 in the rendered
//! prompt) so a provider adapter can place a cache breakpoint at exactly that boundary. The
//! assembler is provider-neutral: it names no vendor. Anthropic consumes the prefix length via
//! a `cache_control` breakpoint; DeepSeek/GLM cache the identical prefix automatically.
//!
//! DETERMINISM CONTRACT: the caller must pass Layer-1 and Layer-2 content that is already
//! deterministically serialized (no timestamps, no nondeterministic map/set iteration; use
//! sorted/ordered iteration). This type does not sort for you; it only guarantees that, given
//! stable layer inputs, the rendered prefix is byte-stable and its length is reported exactly.
//!
//! Source: `docs/plans/2026-07-05_prompt-hardening-and-governance-kernel.md` (the phase-2
//! cache-layering section) and the scratch design note folded into it.

/// The separator between adjacent layers in a rendered prompt. A blank line keeps the layers
/// visually distinct without introducing any volatile content.
const LAYER_SEP: &str = "\n\n";

/// A prompt assembled in strict geological order: Layer 1 (global immutable) at the top,
/// Layer 2 (epic/session context) in the middle, Layer 3 (volatile execution state) at the
/// bottom. Pure + deterministic: no I/O, no timestamps, no nondeterministic iteration.
///
/// Build it with [`LayeredPrompt::new`], set the optional grounding layer with
/// [`LayeredPrompt::with_grounding`], then call [`LayeredPrompt::render`] for the full prompt
/// or [`LayeredPrompt::stable_prefix_len`] for the byte length of the cacheable prefix.
#[derive(Debug, Clone)]
pub struct LayeredPrompt {
    /// Layer 1: the global-immutable block (kernel + role + project rules/schemas). Identical
    /// across every call for a project.
    layer1_global: String,
    /// Layer 2: the epic/session grounding block. Empty when the caller has no grounding.
    /// Changes only every few days.
    layer2_grounding: String,
    /// Layer 3: the volatile execution state (story + latest toolchain/gate error + turn
    /// request). Different on almost every call.
    layer3_volatile: String,
}

impl LayeredPrompt {
    /// Start a layered prompt from the Layer-1 (global immutable) block and the Layer-3
    /// (volatile) block. Layer 2 (grounding) is empty until [`Self::with_grounding`] sets it.
    ///
    /// Surrounding whitespace on each layer is trimmed so the rendered boundaries are stable
    /// regardless of incidental trailing newlines in the inputs (which would otherwise perturb
    /// the byte offset of the cache boundary).
    pub fn new(layer1_global: impl Into<String>, layer3_volatile: impl Into<String>) -> Self {
        Self {
            layer1_global: layer1_global.into().trim().to_string(),
            layer2_grounding: String::new(),
            layer3_volatile: layer3_volatile.into().trim().to_string(),
        }
    }

    /// Set the Layer-2 grounding block (repo digest + rule context). An empty or
    /// whitespace-only value leaves Layer 2 absent, so the prefix is just Layer 1.
    pub fn with_grounding(mut self, grounding: impl Into<String>) -> Self {
        self.layer2_grounding = grounding.into().trim().to_string();
        self
    }

    /// The rendered STABLE PREFIX: Layers 1+2 in order, exactly as they appear at the start of
    /// [`Self::render`]. This is the text a prefix cache should reuse across calls. It contains
    /// NO Layer-3 content, so it is byte-identical for any two prompts sharing the same Layer-1
    /// and Layer-2 inputs.
    pub fn stable_prefix(&self) -> String {
        if self.layer2_grounding.is_empty() {
            self.layer1_global.clone()
        } else {
            format!("{}{LAYER_SEP}{}", self.layer1_global, self.layer2_grounding)
        }
    }

    /// The byte length of the stable prefix (Layers 1+2) within the rendered prompt. A provider
    /// adapter places its cache breakpoint at this offset: everything before it is the cacheable
    /// prefix, everything after it is the volatile Layer-3 tail. Equal to
    /// `self.stable_prefix().len()` and always a valid UTF-8 char boundary (it ends on the last
    /// byte of Layer 2, or Layer 1 when there is no grounding).
    pub fn stable_prefix_len(&self) -> usize {
        self.stable_prefix().len()
    }

    /// Render the full prompt: Layer 1, then Layer 2 (when present), then Layer 3. The first
    /// `stable_prefix_len()` bytes are exactly [`Self::stable_prefix`]; the remainder is the
    /// separator plus the volatile Layer-3 block.
    pub fn render(&self) -> String {
        let prefix = self.stable_prefix();
        if self.layer3_volatile.is_empty() {
            prefix
        } else {
            format!("{prefix}{LAYER_SEP}{}", self.layer3_volatile)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_places_layers_in_geological_order() {
        let p = LayeredPrompt::new("L1-KERNEL", "L3-STORY").with_grounding("L2-GROUNDING");
        let out = p.render();
        let i1 = out.find("L1-KERNEL").expect("layer 1");
        let i2 = out.find("L2-GROUNDING").expect("layer 2");
        let i3 = out.find("L3-STORY").expect("layer 3");
        assert!(i1 < i2 && i2 < i3, "layers must render 1 -> 2 -> 3: {out}");
    }

    #[test]
    fn stable_prefix_is_layers_1_and_2_only_and_excludes_volatile() {
        let p = LayeredPrompt::new("L1", "VOLATILE-TAIL").with_grounding("L2");
        let prefix = p.stable_prefix();
        assert!(prefix.contains("L1"));
        assert!(prefix.contains("L2"));
        assert!(
            !prefix.contains("VOLATILE-TAIL"),
            "the stable prefix must never contain Layer-3 content"
        );
    }

    #[test]
    fn stable_prefix_len_matches_prefix_and_marks_render_boundary() {
        let p = LayeredPrompt::new("kernel-block", "story-and-error")
            .with_grounding("grounding-block");
        let n = p.stable_prefix_len();
        let rendered = p.render();
        assert_eq!(n, p.stable_prefix().len());
        // The first n bytes of the full render are byte-identical to the stable prefix.
        assert_eq!(&rendered.as_bytes()[..n], p.stable_prefix().as_bytes());
    }

    /// THE core cache-layering invariant (design requirement #2): the static prefix is
    /// byte-identical across two builds that differ ONLY in the Layer-3 input.
    #[test]
    fn stable_prefix_is_byte_identical_across_differing_layer3() {
        let l1 = "=== KERNEL ===\nrule 1\nrule 2";
        let l2 = "=== GROUNDING ===\nrepo: o/r\nLanguages: Rust";

        let a = LayeredPrompt::new(l1, "story A + compiler error alpha").with_grounding(l2);
        let b = LayeredPrompt::new(l1, "a COMPLETELY different story B + gate error beta")
            .with_grounding(l2);

        // Prefixes identical byte-for-byte...
        assert_eq!(a.stable_prefix(), b.stable_prefix());
        assert_eq!(a.stable_prefix_len(), b.stable_prefix_len());
        // ...even though the full prompts differ (Layer 3 changed).
        assert_ne!(a.render(), b.render());
        // ...and the differing content is strictly AFTER the cache boundary.
        let n = a.stable_prefix_len();
        assert_eq!(&a.render().as_bytes()[..n], &b.render().as_bytes()[..n]);
    }

    #[test]
    fn no_grounding_makes_the_prefix_just_layer1() {
        let p = LayeredPrompt::new("L1-ONLY", "L3");
        assert_eq!(p.stable_prefix(), "L1-ONLY");
        assert_eq!(p.stable_prefix_len(), "L1-ONLY".len());
        assert!(p.render().contains("L1-ONLY"));
        assert!(p.render().contains("L3"));
    }

    #[test]
    fn incidental_trailing_whitespace_does_not_perturb_the_boundary() {
        let clean = LayeredPrompt::new("L1", "L3").with_grounding("L2");
        let noisy = LayeredPrompt::new("L1\n\n", "  L3  ").with_grounding("\nL2\n  ");
        // Trimming means the two assemble to the same stable prefix + boundary.
        assert_eq!(clean.stable_prefix(), noisy.stable_prefix());
        assert_eq!(clean.stable_prefix_len(), noisy.stable_prefix_len());
    }

    #[test]
    fn empty_volatile_renders_just_the_prefix() {
        let p = LayeredPrompt::new("L1", "").with_grounding("L2");
        assert_eq!(p.render(), p.stable_prefix());
    }
}
