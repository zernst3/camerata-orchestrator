//! OpenRouter provider-safety UI (Pass 2): pure view logic for the "Data safety" settings
//! toggle, the always-visible testing-mode banner, and the per-model provider picker. No
//! rendering-framework dependency (RUST-HEADLESS-CORE-1); the Dioxus adapter (`camerata-ui`)
//! renders this state and owns the HTTP calls to the Pass-1 backend
//! (`GET /api/settings` / `POST /api/settings/provider-policy` /
//! `GET /api/models/providers`).
//!
//! See `docs/design/2026-07-28_openrouter-provider-safety.md` for the full design — this
//! module implements §3 (toggle), §4 (banner), and §5 (picker)'s pure derivations. The
//! actual safety ENFORCEMENT lives entirely server-side in
//! `camerata_llm::provider_policy::provider_constraint_for_request`; nothing here can affect
//! it — a bug in this module can misrender the UI but can never widen what gets sent to
//! OpenRouter.

// ── §4: the always-visible testing-mode banner ──────────────────────────────────

/// Whether the global "TESTING MODE" banner should render. Trivial (`!safe_mode`), but kept
/// as a named, tested function so the one-word inversion can never silently flip at a call
/// site — the banner is safety-critical enough to deserve its own assertion surface.
pub fn testing_mode_banner_visible(safe_mode: bool) -> bool {
    !safe_mode
}

/// The banner's exact copy — a single source of truth so the SSR test and the render call
/// site can't drift from each other.
pub const TESTING_MODE_BANNER_TEXT: &str =
    "⚠ TESTING MODE — data safety OFF. Prompts may be retained or trained on. Do not use with client repositories.";

/// The confirm-dialog copy shown when the operator turns safe mode OFF (§3).
pub const SAFE_MODE_OFF_CONFIRM_TEXT: &str =
    "This sends prompts to providers that may retain or train on them. Never use with client code.";

// ── §5: the provider picker ──────────────────────────────────────────────────────

/// One provider's picker-facing record — the wire shape from
/// `GET /api/models/providers?model_id=<id>`. Mirrors
/// `camerata_llm::model_registry::ProviderEndpointInfo` field-for-field (via the server's
/// `ProviderEndpointWire`).
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct ProviderOption {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub price_in: f64,
    #[serde(default)]
    pub price_out: f64,
    /// Mirrors the server's fail-closed convention (`model_registry.rs`'s
    /// `join_endpoints_with_policy`): a data-policy field genuinely missing from the wire
    /// payload (malformed/partial JSON — the real server always sends both explicitly)
    /// defaults to `true` (unsafe), never `false`. A display bug can therefore only ever
    /// UNDER-claim safety, never show a false safe checkmark.
    #[serde(default = "default_unsafe_flag")]
    pub training: bool,
    #[serde(default = "default_unsafe_flag")]
    pub retains_prompts: bool,
}

fn default_unsafe_flag() -> bool {
    true
}

impl ProviderOption {
    /// Camerata's safe-mode bar: no training AND no prompt retention. Mirrors
    /// `ProviderEndpointInfo::is_safe` (the Pass-1 definition) exactly — this is a VIEW of
    /// the same fact, not a second definition of it.
    pub fn is_safe(&self) -> bool {
        !self.training && !self.retains_prompts
    }
}

/// The wire shape from `GET /api/models/providers?model_id=<id>`.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct ProviderEndpointsResp {
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub providers: Vec<ProviderOption>,
}

/// One row the picker renders. `slug: None` is the "Auto — cheapest safe" row.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderRow {
    /// `None` = "Auto — cheapest safe" (maps to `pinned_provider = None`). `Some(slug)` = a
    /// specific provider pin.
    pub slug: Option<String>,
    pub label: String,
    /// Region flag + price text, e.g. "🇺🇸 US · $0.14/1M in · $0.28/1M out". Empty for Auto.
    pub detail: String,
    /// `true` when this provider (or, for Auto, always) passes the no-train/no-retain bar.
    pub is_safe: bool,
    /// `false` in safe mode for an unsafe provider — the row must render disabled/greyed
    /// ("requires testing mode"). Always `true` in testing mode, and always `true` for Auto.
    pub selectable: bool,
    /// Whether this row matches the currently-pinned provider (or, for Auto, whether no pin
    /// is set).
    pub selected: bool,
}

/// Human copy for a disabled unsafe row, so the label text lives in one place.
pub const UNSAFE_ROW_DISABLED_HINT: &str = "requires testing mode";

/// Map a region/headquarters code to a flag emoji + the code itself, e.g. `"US"` ->
/// `"🇺🇸 US"`. Falls back to the bare code (or an empty string) for anything not in the
/// small known set — OpenRouter's `headquarters` field is not a closed enum, so this is
/// deliberately not exhaustive; an unmapped code still displays as text, never blank.
pub fn region_flag(region: Option<&str>) -> String {
    let Some(code) = region.map(str::trim).filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let upper = code.to_ascii_uppercase();
    let flag = match upper.as_str() {
        "US" => "🇺🇸",
        "SG" => "🇸🇬",
        "DE" => "🇩🇪",
        "FR" => "🇫🇷",
        "GB" | "UK" => "🇬🇧",
        "JP" => "🇯🇵",
        "CA" => "🇨🇦",
        "CN" => "🇨🇳",
        "NL" => "🇳🇱",
        "AU" => "🇦🇺",
        "CH" => "🇨🇭",
        "IN" => "🇮🇳",
        _ => "",
    };
    if flag.is_empty() {
        code.to_string()
    } else {
        format!("{flag} {upper}")
    }
}

/// Format a per-million-token price for the picker row, e.g. `0.14` -> `"$0.14/1M"`.
pub fn format_price_per_million(usd: f64) -> String {
    format!("${usd:.2}/1M")
}

/// Build the picker's rows: the "Auto — cheapest safe" row first, then one row per provider
/// in `providers`, in the order given (callers should pass them cheapest-first if they want
/// that ordering — this function does not sort). Pure; the Dioxus adapter is the only
/// caller that touches the network (fetching `providers` and posting the pin).
///
/// - `safe_mode` ON: unsafe rows are `selectable: false`.
/// - `safe_mode` OFF (testing mode): every row is `selectable: true`.
/// - `selected` marks the Auto row when `pinned` is `None`, or the matching provider row
///   when `pinned` is `Some(slug)`. If `pinned` names a provider not present in `providers`
///   (stale/unknown pin), NO row is marked selected — the picker simply shows no active
///   selection rather than fabricating one; the server-side enforcement already drops an
///   unsafe/unknown pin independently (see `provider_constraint_for_request`), so this is a
///   display nuance only, never a safety concern.
pub fn build_provider_rows(
    providers: &[ProviderOption],
    safe_mode: bool,
    pinned: Option<&str>,
) -> Vec<ProviderRow> {
    let mut rows = Vec::with_capacity(providers.len() + 1);
    rows.push(ProviderRow {
        slug: None,
        label: "Auto — cheapest safe".to_string(),
        detail: String::new(),
        is_safe: true,
        selectable: true,
        selected: pinned.is_none(),
    });
    for p in providers {
        let is_safe = p.is_safe();
        let selectable = if safe_mode { is_safe } else { true };
        let region = region_flag(p.region.as_deref());
        let detail = if region.is_empty() {
            format!(
                "{} in / {} out",
                format_price_per_million(p.price_in),
                format_price_per_million(p.price_out)
            )
        } else {
            format!(
                "{region} · {} in / {} out",
                format_price_per_million(p.price_in),
                format_price_per_million(p.price_out)
            )
        };
        rows.push(ProviderRow {
            slug: Some(p.slug.clone()),
            label: p.name.clone(),
            detail,
            is_safe,
            selectable,
            selected: pinned == Some(p.slug.as_str()),
        });
    }
    rows
}

/// Whether the picker should show "provider data unavailable" and fall back to Auto —
/// true when the endpoint fetch resolved but returned nothing (no key configured, fetch
/// failed, or the model genuinely has no OpenRouter provider data). `None` (fetch still in
/// flight / not yet attempted) is the loading state, not the unavailable state — callers
/// distinguish those with an `Option`, this function only judges the resolved-empty case.
pub fn provider_data_unavailable(providers: &[ProviderOption]) -> bool {
    providers.is_empty()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── §4: banner visibility ──────────────────────────────────────────────────

    #[test]
    fn banner_visible_iff_safe_mode_off() {
        assert!(testing_mode_banner_visible(false));
        assert!(!testing_mode_banner_visible(true));
    }

    #[test]
    fn banner_text_carries_the_required_warnings() {
        assert!(TESTING_MODE_BANNER_TEXT.contains("TESTING MODE"));
        assert!(TESTING_MODE_BANNER_TEXT.contains("retained or trained"));
        assert!(TESTING_MODE_BANNER_TEXT.contains("client repositories"));
    }

    #[test]
    fn confirm_text_warns_about_client_code() {
        assert!(SAFE_MODE_OFF_CONFIRM_TEXT.to_lowercase().contains("client code"));
    }

    // ── ProviderOption::is_safe ────────────────────────────────────────────────

    fn provider(slug: &str, training: bool, retains: bool) -> ProviderOption {
        ProviderOption {
            slug: slug.to_string(),
            name: slug.to_string(),
            region: None,
            price_in: 1.0,
            price_out: 2.0,
            training,
            retains_prompts: retains,
        }
    }

    #[test]
    fn is_safe_requires_both_flags_false() {
        assert!(provider("deepinfra", false, false).is_safe());
        assert!(!provider("deepseek", true, false).is_safe());
        assert!(!provider("streamlake", false, true).is_safe());
        assert!(!provider("bad", true, true).is_safe());
    }

    // ── region_flag ─────────────────────────────────────────────────────────────

    #[test]
    fn region_flag_maps_known_codes() {
        assert_eq!(region_flag(Some("US")), "🇺🇸 US");
        assert_eq!(region_flag(Some("sg")), "🇸🇬 SG"); // case-insensitive
    }

    #[test]
    fn region_flag_falls_back_to_bare_code_for_unknown() {
        assert_eq!(region_flag(Some("XX")), "XX");
    }

    #[test]
    fn region_flag_empty_for_none_or_blank() {
        assert_eq!(region_flag(None), "");
        assert_eq!(region_flag(Some("   ")), "");
    }

    // ── format_price_per_million ──────────────────────────────────────────────

    #[test]
    fn formats_price_to_two_decimals_with_unit() {
        assert_eq!(format_price_per_million(0.14), "$0.14/1M");
        assert_eq!(format_price_per_million(10.0), "$10.00/1M");
    }

    // ── build_provider_rows: Auto row ──────────────────────────────────────────

    #[test]
    fn auto_row_is_always_first_safe_and_selectable() {
        let rows = build_provider_rows(&[], true, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, None);
        assert_eq!(rows[0].label, "Auto — cheapest safe");
        assert!(rows[0].is_safe);
        assert!(rows[0].selectable);
    }

    #[test]
    fn auto_row_selected_iff_no_pin() {
        let providers = vec![provider("deepinfra", false, false)];
        let rows_no_pin = build_provider_rows(&providers, true, None);
        assert!(rows_no_pin[0].selected, "Auto selected when pin is None");

        let rows_pinned = build_provider_rows(&providers, true, Some("deepinfra"));
        assert!(!rows_pinned[0].selected, "Auto NOT selected when a pin is set");
    }

    // ── build_provider_rows: safety-badge / disabling ──────────────────────────

    #[test]
    fn safety_badge_matches_is_safe_definition() {
        let providers = vec![
            provider("deepinfra", false, false), // safe
            provider("deepseek", true, false),   // trains
            provider("streamlake", false, true), // retains
        ];
        let rows = build_provider_rows(&providers, false, None);
        // rows[0] is Auto; provider rows follow in input order.
        assert!(rows[1].is_safe);
        assert!(!rows[2].is_safe);
        assert!(!rows[3].is_safe);
    }

    #[test]
    fn unsafe_rows_disabled_in_safe_mode() {
        let providers = vec![
            provider("deepinfra", false, false),
            provider("deepseek", true, false),
        ];
        let rows = build_provider_rows(&providers, true, None);
        let deepinfra = rows.iter().find(|r| r.slug.as_deref() == Some("deepinfra")).unwrap();
        let deepseek = rows.iter().find(|r| r.slug.as_deref() == Some("deepseek")).unwrap();
        assert!(deepinfra.selectable, "safe provider stays selectable in safe mode");
        assert!(!deepseek.selectable, "unsafe provider must be disabled in safe mode");
    }

    #[test]
    fn all_rows_selectable_in_testing_mode() {
        let providers = vec![
            provider("deepinfra", false, false),
            provider("deepseek", true, false),
            provider("streamlake", false, true),
        ];
        let rows = build_provider_rows(&providers, false, None);
        assert!(rows.iter().all(|r| r.selectable), "testing mode: nothing is disabled");
    }

    // ── build_provider_rows: selection + pin round-trip ────────────────────────

    #[test]
    fn picking_a_provider_marks_exactly_that_row_selected() {
        let providers = vec![
            provider("deepinfra", false, false),
            provider("novita", false, false),
        ];
        let rows = build_provider_rows(&providers, true, Some("novita"));
        let deepinfra = rows.iter().find(|r| r.slug.as_deref() == Some("deepinfra")).unwrap();
        let novita = rows.iter().find(|r| r.slug.as_deref() == Some("novita")).unwrap();
        assert!(!deepinfra.selected);
        assert!(novita.selected);
    }

    #[test]
    fn stale_pin_not_in_providers_selects_nothing() {
        let providers = vec![provider("deepinfra", false, false)];
        let rows = build_provider_rows(&providers, true, Some("not-a-real-slug"));
        assert!(rows.iter().all(|r| !r.selected), "no row fabricates a match for a stale pin");
    }

    #[test]
    fn detail_includes_region_and_both_prices_when_region_present() {
        let mut p = provider("deepinfra", false, false);
        p.region = Some("US".to_string());
        p.price_in = 0.14;
        p.price_out = 0.28;
        let rows = build_provider_rows(std::slice::from_ref(&p), true, None);
        let row = &rows[1];
        assert!(row.detail.contains("🇺🇸 US"));
        assert!(row.detail.contains("$0.14/1M"));
        assert!(row.detail.contains("$0.28/1M"));
    }

    #[test]
    fn detail_omits_region_when_absent() {
        let p = provider("deepinfra", false, false);
        let rows = build_provider_rows(std::slice::from_ref(&p), true, None);
        let row = &rows[1];
        assert!(!row.detail.contains("🇺🇸"));
        assert!(row.detail.contains("in"));
        assert!(row.detail.contains("out"));
    }

    // ── provider_data_unavailable ───────────────────────────────────────────────

    #[test]
    fn unavailable_iff_empty() {
        assert!(provider_data_unavailable(&[]));
        assert!(!provider_data_unavailable(&[provider("deepinfra", false, false)]));
    }

    // ── ProviderEndpointsResp deserialization (the wire contract) ─────────────

    #[test]
    fn provider_endpoints_resp_parses_the_server_shape() {
        let json = r#"{
            "model_id": "deepseek/deepseek-chat",
            "providers": [
                {"slug":"deepinfra","name":"DeepInfra","region":"US","price_in":0.14,"price_out":0.28,"training":false,"retains_prompts":false},
                {"slug":"deepseek","name":"DeepSeek","region":null,"price_in":0.07,"price_out":0.28,"training":true,"retains_prompts":true}
            ]
        }"#;
        let resp: ProviderEndpointsResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model_id, "deepseek/deepseek-chat");
        assert_eq!(resp.providers.len(), 2);
        assert!(resp.providers[0].is_safe());
        assert!(!resp.providers[1].is_safe());
    }

    #[test]
    fn provider_endpoints_resp_tolerates_missing_optional_fields() {
        let json = r#"{"model_id":"m","providers":[{"slug":"x","name":"X"}]}"#;
        let resp: ProviderEndpointsResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.providers[0].region, None);
        assert_eq!(resp.providers[0].price_in, 0.0);
        assert!(
            !resp.providers[0].is_safe(),
            "a training/retains_prompts field missing from the wire payload must resolve to \
             UNSAFE, never a false safe checkmark (fail-closed display, mirroring the server)"
        );
    }
}
