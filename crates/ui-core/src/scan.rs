//! Scan-surface formatting helpers, extracted from the scan UI. Pure string/number formatting with no
//! rendering-framework dependency, unit-tested here.

/// Human-readable token count by magnitude (900 -> "900", 2_000 -> "2k", 2_000_000 -> "2.0M").
pub fn human_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.0}k", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

/// Display label for a deterministic scan tool (known tools get a friendly name; others pass through).
pub fn det_tool_label(tool: &str) -> String {
    match tool {
        "floor" => "Security floor".to_string(),
        "unrouted" => "Unrouted rules".to_string(),
        other => other.to_string(),
    }
}

/// The default triage/finding status (`"active"`).
pub fn default_finding_status() -> String {
    "active".to_string()
}

/// Estimate the token + dollar cost of a governed audit run over `code_chars` of code with
/// `selected` rules, returning `(total_tokens, dollars, passes)`. PURE so the readout is
/// unit-testable without rendering. Pricing is per-million-token model rates; the mode string
/// selects `sequential` / `parallel` / `batch` pass shaping + cache/batch discounts, and the
/// `thorough` / `incremental` / `deep` flags scale the calibration, incremental-scope, and
/// deep-tier passes respectively. Biased HIGH on purpose (see `FUDGE`): an audit that costs
/// more than quoted is the bad surprise.
#[allow(clippy::too_many_arguments)]
pub fn estimate_audit_cost(
    code_chars: usize,
    selected: usize,
    mode: &str,
    audit_in: f64,
    audit_out: f64,
    calib_in: f64,
    calib_out: f64,
    thorough: bool,
    incremental: bool,
    deep: bool,
) -> (u64, f64, usize) {
    const CHUNK_DIGEST_CHARS: usize = 350_000;
    const RULE_BATCH_SIZE: usize = 15;
    const CHARS_PER_TOKEN: f64 = 4.0;
    // Per-pass overhead (rules block + system prompt) that varies per batch and is never
    // cached. The digest + repo map form the cached prefix, so only this remainder is
    // re-sent at full price for subsequent batches.
    const OVERHEAD_CHARS_PER_PASS: usize = 10_000;
    // Output is findings: a baseline per pass plus a term that scales with code scanned
    // (so a findings-dense or large scan isn't under-counted on the half that bites most).
    const OUT_TOKENS_PER_PASS: f64 = 2_200.0;
    const OUTPUT_PER_CODE_TOKEN: f64 = 0.02;
    // Resolution round + general conservatism. Biased HIGH on purpose: logged real runs
    // (budget-mini ~2.24×, chorale ~1.75×) came in UNDER estimate even before caching, and
    // an audit that costs more than quoted is the bad surprise.
    const FUDGE: f64 = 1.4;
    // Prompt-cache pricing multipliers (Anthropic list pricing as of 2024-07):
    //   write (first batch per chunk): 1.25× input
    //   read  (subsequent batches):    0.10× input
    const CACHE_WRITE_MULT: f64 = 1.25;
    const CACHE_READ_MULT: f64 = 0.10;
    // Deep tier (#55): three EXTRA whole-repo passes (SOC-2 gap, deep security, threat model).
    // Each reads the full code once and emits a long prose report. Priced at the audit model.
    const DEEP_PASSES: f64 = 3.0;
    // A deep pass emits far more prose than a per-rule finding pass (full report per lens).
    const DEEP_OUT_TOKENS_PER_PASS: f64 = 8_000.0;

    // Batch mode (#61): the Anthropic Message Batches API charges a flat 50% discount on
    // ALL input and output tokens for the SCAN passes (which are submitted as a batch).
    // The calibration pass always runs real-time (a single call over aggregated findings
    // — not batched), so calib pricing is NOT discounted.
    let batch_discount = if mode == "batch" { 0.5 } else { 1.0 };
    let (eff_audit_in, eff_audit_out) = (audit_in * batch_discount, audit_out * batch_discount);
    // Calibration is real-time even in batch mode: one call over the aggregated findings.
    let (eff_calib_in, eff_calib_out) = (calib_in, calib_out);

    let chunks = code_chars.div_ceil(CHUNK_DIGEST_CHARS).max(1);
    let batches = if mode == "sequential" {
        1
    } else {
        selected.div_ceil(RULE_BATCH_SIZE).max(1)
    };
    let passes = chunks * batches;
    let code_tokens = code_chars as f64 / CHARS_PER_TOKEN;

    // ── Scan passes, priced at the AUDIT model (with batch discount applied) ──
    //
    // Without caching: the full digest is re-sent at full input price every pass.
    // With caching (parallel/batch mode, batches > 1): per chunk, batch 0 pays full input
    // + the one-time 1.25× cache-write surcharge; batches 1..N read the cached digest at
    // 0.1×. Sequential (batches == 1) has no reuse, so no discount.
    //
    // Overhead tokens (rules block, system prompt) are always sent at full price since they
    // vary per batch.
    let scan_in = if batches <= 1 {
        // No caching benefit: every batch pays full price for the digest.
        (code_chars * batches + OVERHEAD_CHARS_PER_PASS * passes) as f64 / CHARS_PER_TOKEN
    } else {
        // Batch 0 per chunk: full digest price + cache-write surcharge.
        // Batches 1..N per chunk: digest at cache-read rate (0.1×).
        let digest_tokens_per_chunk = code_chars as f64 / chunks as f64 / CHARS_PER_TOKEN;
        let write_cost = digest_tokens_per_chunk * CACHE_WRITE_MULT * chunks as f64;
        let read_cost = digest_tokens_per_chunk
            * CACHE_READ_MULT
            * (batches.saturating_sub(1)) as f64
            * chunks as f64;
        // Overhead (never cached) is full price for every pass.
        let overhead_cost = OVERHEAD_CHARS_PER_PASS as f64 / CHARS_PER_TOKEN * passes as f64;
        write_cost + read_cost + overhead_cost
    };
    let scan_out =
        OUT_TOKENS_PER_PASS * passes as f64 + OUTPUT_PER_CODE_TOKEN * code_tokens * batches as f64;

    // ── Calibration: ONE pass over all findings, priced at the CALIBRATION model. It
    // re-reads roughly the scan's output (the findings) and RE-EMITS each finding with a
    // corrected/verified body. So its output rides with the full findings volume, ~1× the
    // scan's output. Thorough mode (#51) runs ~3× for multi-vote consensus.
    let cal_passes = if thorough { 3.0 } else { 1.0 };
    let cal_in = scan_out * cal_passes;
    let cal_out = scan_out * cal_passes;

    // ── Deep tier: three EXTRA whole-repo prose passes at the AUDIT model. Each reads the
    // full code (no per-rule batching, no caching discount — distinct prompts per lens) and
    // emits a long prose report. This is the dominant cost when enabled, which is why deep is
    // surfaced as the priciest option in the readout. Batch discount does NOT apply (these run
    // real-time as part of the deep lens flow, not in the scan batch).
    let (deep_in, deep_out) = if deep {
        let full_code_tokens = code_chars as f64 / CHARS_PER_TOKEN;
        let din = full_code_tokens * DEEP_PASSES;
        let dout = DEEP_OUT_TOKENS_PER_PASS * DEEP_PASSES;
        (din, dout)
    } else {
        (0.0, 0.0)
    };

    // Incremental scope (only changed files actually billed) would lower the scan portion, but
    // the client has no changed-file token breakdown today (see fn doc + followup), so we keep
    // the full-scan price and let the readout flag incremental as an over-estimate. Bind the
    // flag so its role is explicit even though the number is unchanged here.
    let _ = incremental;

    let dollars = ((scan_in * eff_audit_in + scan_out * eff_audit_out)
        + (cal_in * eff_calib_in + cal_out * eff_calib_out)
        + (deep_in * audit_in + deep_out * audit_out))
        / 1_000_000.0
        * FUDGE;
    let total_tokens =
        ((scan_in + scan_out + cal_in + cal_out + deep_in + deep_out) * FUDGE) as u64;
    (total_tokens, dollars, passes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_tokens_formats_by_magnitude() {
        assert_eq!(human_tokens(900), "900");
        assert_eq!(human_tokens(2_000), "2k");
        assert_eq!(human_tokens(350_000), "350k");
        assert_eq!(human_tokens(2_000_000), "2.0M");
    }

    #[test]
    fn det_tool_label_maps_known_and_passes_through_unknown() {
        assert_eq!(det_tool_label("floor"), "Security floor");
        assert_eq!(det_tool_label("unrouted"), "Unrouted rules");
        assert_eq!(det_tool_label("clippy"), "clippy");
        // (the "ruff" passthrough case was a duplicate test in cockpit.rs; merged here.)
        assert_eq!(det_tool_label("ruff"), "ruff");
    }

    #[test]
    fn default_finding_status_is_active() {
        assert_eq!(default_finding_status(), "active");
    }

    // ── estimate_audit_cost — pricing model. Moved from the cockpit + scan UI; all
    // assertions preserved. Pure math, no VirtualDom. ─────────────────────────────

    /// Sequential mode (1 batch per chunk) has no caching reuse across batches — the
    /// estimate must match the pre-caching math (full digest price every pass).
    #[test]
    fn sequential_mode_no_cache_discount() {
        // Small repo: 100k chars, 0 rules, sequential.
        let (toks, dollars, passes) =
            estimate_audit_cost(100_000, 0, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes, 1, "0 rules + sequential = one pass");
        assert!(toks > 0, "some tokens");
        assert!(dollars > 0.0, "some cost");
    }

    /// Parallel mode with multiple batches should cost LESS than the naive per-batch full
    /// price because subsequent batches read the digest from cache at ~0.1×.
    #[test]
    fn parallel_multi_batch_cheaper_than_sequential_sum() {
        // 30 rules -> ceil(30/15)=2 batches; 350k chars = 1 chunk.
        let (_, dollars_parallel, passes_parallel) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes_parallel, 2, "2 batches for 30 rules");

        // If we ran sequential with 30 rules we get 1 pass; run twice to simulate
        // the naive "pay full price twice" baseline.
        let (_, dollars_seq_single, _) =
            estimate_audit_cost(350_000, 30, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let naive_two_passes = dollars_seq_single * 2.0;

        assert!(
            dollars_parallel < naive_two_passes,
            "caching makes 2 parallel batches cheaper than naive 2× sequential: {dollars_parallel:.4} < {naive_two_passes:.4}"
        );
    }

    /// Single-batch parallel (1 rule, or 0 rules) has nothing to cache — no second batch
    /// to amortise over, so the discount path is not taken.
    #[test]
    fn parallel_single_batch_no_discount() {
        // 1 rule -> 1 batch in parallel mode.
        let (toks1, dollars1, passes1) =
            estimate_audit_cost(350_000, 1, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (toks_seq, dollars_seq, passes_seq) =
            estimate_audit_cost(350_000, 1, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes1, 1);
        assert_eq!(passes_seq, 1);
        // Token counts should be in the same ballpark (both are 1 pass over the same chunk).
        // The cache-write surcharge on the parallel path makes it *slightly* higher than
        // sequential, but they should be within 30% of each other.
        let ratio = toks1 as f64 / toks_seq as f64;
        assert!(
            ratio < 1.3,
            "single-batch parallel not much more expensive than sequential: ratio={ratio:.2}"
        );
        let _ = (dollars1, dollars_seq); // exercise the values without asserting exact amounts
    }

    /// Thorough mode triples the calibration cost; the estimate should grow accordingly.
    #[test]
    fn thorough_mode_costs_more_than_default() {
        let (_, dollars_default, _) =
            estimate_audit_cost(200_000, 15, "parallel", 3.0, 15.0, 1.0, 5.0, false, false, false);
        let (_, dollars_thorough, _) =
            estimate_audit_cost(200_000, 15, "parallel", 3.0, 15.0, 1.0, 5.0, true, false, false);
        assert!(
            dollars_thorough > dollars_default,
            "thorough costs more: {dollars_thorough:.4} > {dollars_default:.4}"
        );
    }

    /// Batch mode applies a flat 50% discount to the SCAN passes vs. parallel on the same
    /// config. Calibration is NOT discounted (it always runs real-time). The pass count
    /// is identical (same chunking + rule-batching).
    #[test]
    fn batch_mode_cheaper_than_parallel_due_to_scan_discount() {
        // 30 rules, 350k chars = 1 chunk, 2 rule-batches. Calibration = same model.
        let (_, dollars_parallel, passes_parallel) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, dollars_batch, passes_batch) =
            estimate_audit_cost(350_000, 30, "batch", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(
            passes_parallel, passes_batch,
            "same pass count in parallel and batch (only pricing differs)"
        );
        // Batch must be cheaper than parallel (scan discount applied), but the ratio is
        // not exactly 0.5 because calibration is priced at full rate in both modes.
        assert!(
            dollars_batch < dollars_parallel,
            "batch is cheaper than parallel: {dollars_batch:.4} < {dollars_parallel:.4}"
        );
        // The discount is at least 25% overall (scan dominates in a 2-batch, 1-chunk case).
        let ratio = dollars_batch / dollars_parallel;
        assert!(
            ratio < 0.75,
            "batch should be at least 25% cheaper than parallel: ratio={ratio:.4}"
        );
    }

    /// Batch mode with 0 rules (free-form, 1 pass per chunk): calibration cost is
    /// identical in both modes; scan cost is halved. Total must be cheaper in batch mode.
    #[test]
    fn batch_mode_zero_rules_cheaper_than_parallel() {
        let (_, dollars_parallel, _) =
            estimate_audit_cost(200_000, 0, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, dollars_batch, _) =
            estimate_audit_cost(200_000, 0, "batch", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert!(
            dollars_batch < dollars_parallel,
            "batch cheaper even with 0 rules: {dollars_batch:.4} < {dollars_parallel:.4}"
        );
    }

    /// Deep tier (three extra whole-repo passes) must ADD to the dollar figure, and it must be
    /// the single priciest option vs. thorough or full-vs-incremental on the same config.
    #[test]
    fn deep_tier_costs_more_and_is_the_priciest_option() {
        let base = |deep: bool, thorough: bool| {
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, thorough, false, deep).1
        };
        let standard = base(false, false);
        let thorough = base(false, true);
        let deep = base(true, false);
        assert!(deep > standard, "deep adds cost: {deep:.4} > {standard:.4}");
        assert!(
            deep > thorough,
            "deep is the priciest option (more than thorough): {deep:.4} > {thorough:.4}"
        );
    }

    /// The incremental flag is plumbed through but, with no changed-file breakdown available
    /// client-side, prices the same full-scan figure (over-estimate by design). It must not
    /// blow up the estimate and must equal the full-scan number for the same inputs.
    #[test]
    fn incremental_flag_prices_same_as_full_today() {
        let full =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let incremental =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, true, false);
        assert_eq!(
            full.1, incremental.1,
            "incremental prices the full set today (no changed-file data): {} vs {}",
            full.1, incremental.1
        );
    }

    /// When AI scan is off (run_ai_review=false), the UI passes 0.0 for all model prices.
    /// estimate_audit_cost must return $0 in that case (no LLM calls, no token spend).
    #[test]
    fn ai_scan_off_zero_prices_yields_zero_dollars() {
        // Simulate the UI's behaviour when run_ai_review() is false: both model prices
        // are clamped to (0.0, 0.0) before calling estimate_audit_cost.
        let (toks, dollars, _passes) =
            estimate_audit_cost(350_000, 30, "parallel", 0.0, 0.0, 0.0, 0.0, false, false, false);
        assert_eq!(
            dollars, 0.0,
            "zero prices must produce $0 estimate (AI scan off): got {dollars}"
        );
        // Token count is still computed (for informational display) even at $0.
        assert!(toks > 0, "token count should still be non-zero even when prices are zero");
    }

    /// A free OpenRouter model has price_in=0.0 and price_out=0.0.  Passing those values
    /// must produce a $0 estimate (the model is free, so no cost regardless of token count).
    #[test]
    fn free_model_zero_prices_yields_zero_dollars() {
        let (_, dollars, _) =
            estimate_audit_cost(200_000, 15, "parallel", 0.0, 0.0, 0.0, 0.0, false, false, false);
        assert_eq!(
            dollars, 0.0,
            "free model (price_in=price_out=0) must yield $0 estimate: got {dollars}"
        );
    }

    /// A paid model with known registry prices must produce a non-zero estimate, and the
    /// estimate must scale with price: doubling the model price doubles the dollar figure.
    #[test]
    fn paid_model_registry_prices_produce_nonzero_and_scale_linearly() {
        let (_, dollars_base, _) =
            estimate_audit_cost(200_000, 15, "parallel", 1.0, 5.0, 1.0, 5.0, false, false, false);
        let (_, dollars_double, _) =
            estimate_audit_cost(200_000, 15, "parallel", 2.0, 10.0, 2.0, 10.0, false, false, false);
        assert!(dollars_base > 0.0, "paid model must yield non-zero estimate: {dollars_base}");
        // Doubling prices must double the dollar figure (the function is linear in price).
        let ratio = dollars_double / dollars_base;
        assert!(
            (ratio - 2.0).abs() < 0.001,
            "doubling model prices must double the estimate: ratio={ratio:.4}"
        );
    }

    /// Sonnet 4.6 registry prices ($3/$15 per M) produce a meaningful estimate for a
    /// medium-sized repo scan — sanity-checks the default fallback used by the UI.
    #[test]
    fn sonnet_registry_price_estimate_is_positive() {
        // Sonnet 4.6 list price: $3 in / $15 out per million tokens.
        let (_, dollars, _) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert!(
            dollars > 0.0,
            "Sonnet-priced estimate must be positive for a 350k-char, 30-rule scan: {dollars}"
        );
    }

    // The following three (from the scan UI) assert monotonicity at a distinct input point
    // (400k chars / 20 rules), complementing the cockpit cases above.

    #[test]
    fn estimate_cost_returns_passes_and_nonzero_dollars() {
        let (tokens, dollars, passes) =
            estimate_audit_cost(400_000, 20, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert!(tokens > 0, "tokens estimated");
        assert!(dollars > 0.0, "dollars estimated");
        assert!(passes >= 1, "at least one pass");
    }

    #[test]
    fn estimate_cost_batch_mode_is_cheaper_than_parallel() {
        let (_, parallel, _) =
            estimate_audit_cost(400_000, 20, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, batch, _) =
            estimate_audit_cost(400_000, 20, "batch", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert!(batch < parallel, "batch (50% scan discount) must cost less; batch={batch} parallel={parallel}");
    }

    #[test]
    fn estimate_cost_deep_tier_adds_cost() {
        let (_, without, _) =
            estimate_audit_cost(400_000, 20, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, with_deep, _) =
            estimate_audit_cost(400_000, 20, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, true);
        assert!(with_deep > without, "deep tier must increase the estimate; deep={with_deep} base={without}");
    }
}
