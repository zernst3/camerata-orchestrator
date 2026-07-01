//! The global stylesheet. Kept as one string so the whole look — palette, type
//! scale, spacing, motion — lives in a single, reviewable place.
//!
//! Theme: "Bletchley industrial amber" — dark near-black ground, warm amber
//! accent, Courier Prime monospace titling, Inter sans body.

pub const GLOBAL_CSS: &str = r#"
:root {
  /* ── Bletchley industrial amber palette ─────────────────────────────── */
  /* Text hierarchy */
  --ink:        #f1ede9;   /* --text-main  : warm near-white on dark ground */
  --ink-soft:   #c2b7ad;   /* --text-muted : secondary / labels */
  --ink-faint:  #8c8075;   /* --text-faint : tertiary / hints */

  /* Ground surfaces — dark, slightly warm */
  --paper:      #1a1816;   /* deepest background layer */
  --surface:    rgba(26,24,22, var(--opacity-high)); /* raised cards (glassmorphic) */
  --line:       #2e2a25;   /* --glass-border : hairline borders */
  --line-soft:  #252220;   /* softer inner dividers */

  /* Accent: industrial amber */
  --accent:     #ca8a04;   /* --primary : amber gold */
  --accent-ink: #b07803;   /* darker amber for text / hover */
  --accent-wash:rgba(202,138,4, 0.12); /* faint amber fill */

  /* Semantic signals */
  --good:       #16a34a;   /* --success */
  --warning-color: #ea580c; /* --warning / --cyan (both map to burnt orange) */
  --danger-color:  #dc2626; /* --danger */

  /* Opacity tiers for glassmorphic layering */
  --opacity-high: 0.78;    /* main cards, warning banners */
  --opacity-mid:  0.72;    /* sidebars, dropdown/modal overlays */
  --opacity-low:  0.65;    /* inner nested panels, detail boxes */

  /* Glass system */
  --glass-bg:     rgba(26,24,22, var(--opacity-high));
  --glass-border: #2e2a25;
  --glass-shadow: 0 15px 40px rgba(0,0,0,0.75), inset 0 2px 0 rgba(255,255,255,0.05);

  /* Enterprise-sharp corners (Linear/Vercel/Stripe register), not consumer-round. */
  --r-lg: 12px;
  --r-md: 8px;
  --r-sm: 5px;

  --shadow-card: 0 1px 2px rgba(0,0,0,.18), 0 10px 30px rgba(0,0,0,.28);
  --shadow-pop:  var(--glass-shadow);

  /* Slow, reassuring easing. Nothing snaps. */
  --ease: cubic-bezier(.22,.61,.36,1);

  /* Bombe overlay opacity — TWEAK HERE (lower = bombe more visible) */
  --bombe-overlay-idle-alpha: 0.28;   /* idle:    0.82 was too dark; lower = bombe peeks through more */
  --bombe-overlay-run-alpha:  0.10;   /* running: 0.48 was too dark; lower = bombe glows through clearly */

  /* Page tint — the SINGLE translucent dark layer in front of the Bombe. One value
     for ALL pages' main content (no double-stacking) so no page reads denser/clearer
     than another. Side panels use the denser --rail-tint. NOT the bombe overlay (above). */
  --page-tint: rgba(20,18,17,0.66);   /* main content tint over the Bombe — one per page, never stacked */
  --rail-tint: rgba(20,18,17,0.92);   /* denser side-panel tint (rail / inspector / govdev nav) */

  /* chorale table palette → mapped onto the Bletchley amber scheme so the
     grouped tables read as part of the same dark-industrial surface.
     (chorale exposes these as overridable CSS variables.) */
  --chorale-accent:            var(--accent);
  --chorale-accent-contrast:   #1a1816;
  /* tables use SLIGHTLY TRANSLUCENT dark so the bombe peeks through while cells stay readable.
     Input bg and popovers stay solid for readability / no see-through dropdowns. */
  --chorale-surface:           rgba(22,19,15,0.86);
  --chorale-text:              var(--ink);
  --chorale-text-muted:        var(--ink-soft);
  --chorale-text-subtle:       var(--ink-faint);
  --chorale-text-disabled:     var(--ink-faint);
  --chorale-border:            var(--line);
  --chorale-divider:           var(--line);
  --chorale-separator-color:   var(--line);
  --chorale-header-bg:         rgba(16,13,11,0.9);
  --chorale-group-header-bg:   var(--accent-wash);
  --chorale-group-header-border: var(--line);
  --chorale-toolbar-bg:        rgba(22,19,15,0.86);
  --chorale-input-bg:          #0e0c0b;
  --chorale-input-border:      var(--line);
  --chorale-button-bg:         #1f1b17;
  --chorale-button-disabled-bg: var(--line-soft);
  --chorale-popover-bg:        #16130f;
  --chorale-popover-shadow:    0 15px 40px rgba(0,0,0,0.7);
  --chorale-frozen-divider-shadow: 3px 0 4px -2px rgba(0,0,0,0.5);
  --chorale-range-bg:          var(--accent-wash);
  --chorale-active-cell-outline: var(--accent);
  --chorale-row-selected-divider: var(--accent);
  --chorale-error:             #f87171;
  /* badges — chorale falls back to LIGHT pills if these are unset (the white state badges) */
  --chorale-badge-default-bg:  rgba(255,255,255,0.06);
  --chorale-badge-default-text: var(--ink-soft);
  --chorale-badge-gray-bg:     rgba(140,128,117,0.15);
  --chorale-badge-gray-text:   var(--ink-soft);
  --chorale-badge-green-bg:    rgba(22,163,74,0.18);
  --chorale-badge-green-text:  #4ade80;
  --chorale-badge-red-bg:      rgba(220,38,38,0.18);
  --chorale-badge-red-text:    #f87171;
  --chorale-badge-yellow-bg:   rgba(202,138,4,0.20);
  --chorale-badge-yellow-text: #fbbf24;
}

* { box-sizing: border-box; }

html, body {
  margin: 0;
  padding: 0;
  height: 100%;
  /* The window itself never scrolls — the app fills the viewport exactly and
     scrolling happens inside the content panes (e.g. .cockpit-scroll). This kills
     the persistent ~95%-height page scrollbar. */
  overflow: hidden;
  /* Bletchley: deep near-black radial gradient — darkest at the edges,
     slightly lighter anthracite at the center. */
  background: radial-gradient(circle at center, #242220 0%, #121110 100%);
  color: var(--ink);
  /* Sans body: Inter; title + mono: Courier Prime (industrial monospace). */
  font-family: "Inter", system-ui, -apple-system, BlinkMacSystemFont,
               "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
  line-height: 1.5;            /* let the body breathe */
  font-feature-settings: "cv11", "ss01";
}

/* Typography is the #1 "designed vs default" tell: give headings tighter tracking + a clear
   weight step over body, so type does work instead of sitting at system defaults. */
.h1, .entity-name, .scan-section-h, .scan-stack-repo, .pg-card-name, .onboard-step-h,
.rule-modal-title, .chat-title, .pg-title, .rules-title {
  letter-spacing: -.017em;
  font-weight: 700;
}
.h1 { font-weight: 760; }

.app-root {
  height: 100vh;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  align-items: center;
}

/* ---- progress rail ---------------------------------------------------- */
.rail {
  width: 100%;
  display: flex;
  justify-content: center;
  padding: 26px 0 6px;
}
.rail-inner {
  display: flex;
  gap: 40px;
  align-items: center;
}
.rail-step {
  display: flex;
  align-items: center;
  gap: 9px;
  opacity: .45;
  transition: opacity .6s var(--ease);
}
.rail-step.active { opacity: 1; }
.rail-step.done   { opacity: .8; }
.rail-dot {
  width: 8px; height: 8px;
  border-radius: 50%;
  background: var(--ink-faint);
  transition: background .6s var(--ease), transform .6s var(--ease);
}
.rail-step.active .rail-dot { background: var(--accent); transform: scale(1.35); }
.rail-step.done   .rail-dot { background: var(--good); }
.rail-label {
  font-size: 12.5px;
  letter-spacing: .02em;
  color: var(--ink-soft);
}
.rail-step.active .rail-label { color: var(--ink); }

/* ---- stage / page shell ---------------------------------------------- */
.stage {
  width: 100%;
  display: flex;
  justify-content: center;
  flex: 1;
}
.page {
  width: 100%;
  max-width: 720px;
  padding: 40px 32px 88px;
  animation: rise .7s var(--ease) both;
}
.page-wide { max-width: 90%; width: 90%; }

/* Page/content ENTRANCE animations are disabled for now. They introduced an inconsistent page-
   navigation "pop" (some views animated on mount via `.page { animation: rise }`, others didn't,
   so navigating to/from the Rules page flashed a transient dark layer as the content faded in).
   These four keyframes are kept as NO-OPS — every `animation: rise/fade/slideIn/pop` call site
   stays valid and simply renders instantly. Re-enable later by restoring the from/to states.
   Continuous/functional animations (spinners, the Bombe machine, toasts, LEDs, pulses) are NOT
   affected. (Bonus: a no-op `rise` also has no transform, so it can't make `.page` a containing
   block for fixed-position modals — preserving the modal viewport-centering fix.) */
@keyframes rise {
  from { opacity: 1; }
  to   { opacity: 1; }
}
@keyframes fade {
  from { opacity: 1; }
  to   { opacity: 1; }
}
@keyframes slideIn {
  from { opacity: 1; }
  to   { opacity: 1; }
}

/* ---- type ------------------------------------------------------------- */
.eyebrow {
  font-size: 12.5px;
  letter-spacing: .14em;
  text-transform: uppercase;
  color: var(--accent-ink);
  font-weight: 600;
  margin: 0 0 14px;
}
.h1 {
  font-size: 34px;
  line-height: 1.14;
  letter-spacing: -.02em;
  font-weight: 640;
  margin: 0 0 12px;
}
.lede {
  font-size: 17.5px;
  line-height: 1.6;
  color: var(--ink-soft);
  margin: 0 0 30px;
  max-width: 58ch;
}
.section-label {
  font-size: 14px;
  font-weight: 600;
  color: var(--ink);
  margin: 0 0 4px;
}
.section-hint {
  font-size: 14px;
  color: var(--ink-faint);
  margin: 0 0 16px;
}

/* ---- form bits -------------------------------------------------------- */
.field { margin-bottom: 30px; animation: slideIn .6s var(--ease) both; }
.input, .textarea {
  width: 100%;
  font: inherit;
  font-size: 16px;
  color: var(--ink);
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 14px 16px;
  outline: none;
  transition: border-color .3s var(--ease), box-shadow .3s var(--ease);
}
.input:focus, .textarea:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 4px var(--accent-wash);
}
.textarea { resize: vertical; min-height: 96px; line-height: 1.55; }
.input::placeholder, .textarea::placeholder { color: var(--ink-faint); }

/* card — the rounded surface used for entities, plan nodes, etc. */
.card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 20px 22px;
  box-shadow: var(--shadow-card);
}
.card + .card { margin-top: 16px; }

.entity-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 14px;
}
.entity-name { font-size: 18px; font-weight: 620; letter-spacing: -.01em; }
.entity-kicker { font-size: 13px; color: var(--ink-faint); }

.field-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 9px 0;
  border-top: 1px solid var(--line-soft);
}
.field-name { font-size: 15px; flex: 1; }
.type-glyph {
  width: 26px; height: 26px;
  border-radius: 8px;
  background: var(--paper);
  border: 1px solid var(--line);
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 12px; font-weight: 600; color: var(--ink-soft);
}
.type-label { font-size: 13.5px; color: var(--ink-soft); min-width: 150px; }

/* chips — quick replies + feature tags */
.chips { display: flex; flex-wrap: wrap; gap: 8px; }
.chip {
  font: inherit;
  font-size: 14px;
  color: var(--ink);
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 8px 15px;
  cursor: pointer;
  transition: all .25s var(--ease);
}
.chip:hover { border-color: var(--accent); color: var(--accent-ink); transform: translateY(-1px); }
.chip:active { transform: translateY(0); }
.chip.tag { cursor: default; background: var(--paper); color: var(--ink-soft); font-size: 13px; padding: 5px 12px; }
.chip.tag:hover { transform: none; border-color: var(--line); color: var(--ink-soft); }

/* ---- buttons ---------------------------------------------------------- */
.actions {
  margin-top: 40px;
  display: flex;
  align-items: center;
  gap: 16px;
}
.btn-primary {
  font: inherit;
  font-size: 16px;
  font-weight: 580;
  color: #fff;
  background: var(--accent);
  border: none;
  border-radius: 999px;
  padding: 14px 28px;
  cursor: pointer;
  box-shadow: 0 6px 18px rgba(200,105,74,.28);
  transition: transform .25s var(--ease), box-shadow .25s var(--ease), background .25s var(--ease);
}
.btn-primary:hover { background: var(--accent-ink); transform: translateY(-1px); box-shadow: 0 10px 26px rgba(200,105,74,.34); }
.btn-primary:active { transform: translateY(0); }
.btn-quiet {
  font: inherit;
  font-size: 15px;
  color: var(--ink-soft);
  background: transparent;
  border: none;
  cursor: pointer;
  padding: 12px 6px;
  transition: color .25s var(--ease);
}
.btn-quiet:hover { color: var(--ink); }

/* ---- clarify (hero) --------------------------------------------------- */
.clarify {
  width: 100%;
  max-width: 640px;
  padding: 32px 28px 80px;
  margin: 0 auto;
}
.still {
  display: inline-flex;
  align-items: center;
  gap: 9px;
  font-size: 13px;
  color: var(--ink-soft);
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 7px 14px;
  margin-bottom: 28px;
}
.still-pips { display: inline-flex; gap: 5px; }
.still-pip { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); transition: background .5s var(--ease); }
.still-pip.gone { background: var(--line); }

.transcript { display: flex; flex-direction: column; gap: 18px; }

.bubble { animation: slideIn .55s var(--ease) both; }
.bubble-eng {
  max-width: 90%;
}
.who {
  display: flex; align-items: center; gap: 9px;
  font-size: 12.5px; color: var(--ink-faint); margin-bottom: 7px;
}
.who-avatar {
  width: 22px; height: 22px; border-radius: 50%;
  background: var(--accent-wash); color: var(--accent-ink);
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 11px; font-weight: 700;
}
.q-text {
  font-size: 19px; line-height: 1.45; letter-spacing: -.01em; color: var(--ink);
  margin: 0 0 8px;
}
.q-reason {
  font-size: 14.5px; line-height: 1.55; color: var(--ink-soft);
  margin: 0 0 16px; padding-left: 13px; border-left: 2px solid var(--accent-wash);
}
.bubble-user {
  align-self: flex-end;
  max-width: 78%;
}
.answer {
  background: var(--accent);
  color: #fff;
  border-radius: 18px 18px 6px 18px;
  padding: 11px 17px;
  font-size: 15.5px;
  line-height: 1.45;
  box-shadow: 0 6px 16px rgba(200,105,74,.22);
}

/* the input dock at the bottom of the conversation */
.dock { margin-top: 26px; }
.dock-chips { margin-bottom: 12px; }
.dock-row {
  display: flex; gap: 10px; align-items: flex-end;
}
.dock-input {
  flex: 1;
  font: inherit; font-size: 15.5px; color: var(--ink);
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 16px;
  padding: 13px 16px;
  outline: none;
  transition: border-color .3s var(--ease), box-shadow .3s var(--ease);
}
.dock-input:focus { border-color: var(--accent); box-shadow: 0 0 0 4px var(--accent-wash); }
.dock-send {
  font: inherit; font-size: 15px; font-weight: 560; color: #fff;
  background: var(--accent); border: none; border-radius: 14px;
  padding: 13px 20px; cursor: pointer;
  transition: background .25s var(--ease), transform .2s var(--ease);
}
.dock-send:hover { background: var(--accent-ink); transform: translateY(-1px); }

/* the "Refining the plan…" beat between turns */
.refining {
  display: flex; align-items: center; gap: 11px;
  color: var(--ink-soft); font-size: 14.5px;
  padding: 8px 2px; animation: fade .4s var(--ease) both;
}
.dots { display: inline-flex; gap: 5px; }
.dots i {
  width: 6px; height: 6px; border-radius: 50%; background: var(--accent);
  display: inline-block; animation: pulse 1.3s var(--ease) infinite;
}
.dots i:nth-child(2) { animation-delay: .18s; }
.dots i:nth-child(3) { animation-delay: .36s; }
@keyframes pulse {
  0%, 100% { opacity: .25; transform: translateY(0); }
  40%      { opacity: 1;   transform: translateY(-3px); }
}

/* ---- plan reveal ------------------------------------------------------ */
.plan { animation: rise .8s var(--ease) both; }
.plan-prose {
  font-size: 18px; line-height: 1.65; color: var(--ink);
  margin: 0 0 30px;
}
.plan-map { display: flex; flex-direction: column; gap: 14px; margin-bottom: 8px; }
.plan-node {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 18px 20px;
  box-shadow: var(--shadow-card);
  animation: slideIn .6s var(--ease) both;
}
.plan-node-head { display: flex; align-items: center; gap: 11px; margin-bottom: 12px; }
.plan-node-name { font-size: 17px; font-weight: 620; }
.plan-node-glyph {
  width: 30px; height: 30px; border-radius: 9px;
  background: var(--accent-wash); color: var(--accent-ink);
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 14px; font-weight: 700;
}
.plan-actions { display: flex; flex-wrap: wrap; gap: 7px; margin-bottom: 10px; }
.action-pill {
  font-size: 13px; color: var(--ink-soft);
  background: var(--paper); border: 1px solid var(--line);
  border-radius: 7px; padding: 4px 10px;
}
.plan-note {
  display: flex; align-items: center; gap: 8px;
  font-size: 13.5px; color: var(--accent-ink);
}
.plan-note::before { content: "✓"; color: var(--good); font-weight: 700; }

/* ---- build narrative -------------------------------------------------- */
.build { max-width: 560px; margin: 0 auto; padding-top: 8px; }
.build-list { display: flex; flex-direction: column; gap: 4px; margin-top: 8px; }
.build-stage {
  display: flex; align-items: center; gap: 16px;
  padding: 16px 4px;
  border-bottom: 1px solid var(--line-soft);
  transition: opacity .6s var(--ease);
}
.build-stage.pending { opacity: .32; }
.build-stage.active  { opacity: 1; }
.build-stage.done    { opacity: .7; }
.stage-mark {
  width: 26px; height: 26px; border-radius: 50%;
  display: inline-flex; align-items: center; justify-content: center;
  flex: 0 0 auto;
  border: 1.5px solid var(--line);
  transition: all .5s var(--ease);
}
.build-stage.done .stage-mark {
  background: var(--good); border-color: var(--good); color: #fff;
  animation: pop .5s var(--ease) both;
}
.build-stage.active .stage-mark { border-color: var(--accent); }
/* Disabled for now (see the entrance-animation note above): no-op so `animation: pop` call
   sites render instantly. Restore the scale/opacity frames to re-enable. */
@keyframes pop {
  from { transform: none; opacity: 1; }
  to   { transform: none; opacity: 1; }
}
/* the active spinner: a slow, calm ring */
.spinner {
  width: 15px; height: 15px; border-radius: 50%;
  border: 2px solid var(--accent-wash);
  border-top-color: var(--accent);
  animation: spin 1s linear infinite;
}
@keyframes spin { to { transform: rotate(360deg); } }
.stage-text { font-size: 16.5px; color: var(--ink); }
.build-stage.pending .stage-text { color: var(--ink-soft); }
.build-caption { font-size: 14.5px; color: var(--ink-faint); margin-top: 22px; }

/* ---- live ------------------------------------------------------------- */
.live { max-width: 600px; margin: 0 auto; text-align: center; padding-top: 24px; }
.live-badge {
  width: 76px; height: 76px; border-radius: 50%;
  background: var(--good); color: #fff;
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 34px; margin: 0 auto 26px;
  box-shadow: 0 12px 34px rgba(92,138,92,.32);
  animation: pop .6s var(--ease) both;
}
.live-url {
  display: inline-flex; align-items: center; gap: 10px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 14px 20px;
  font-size: 16px; color: var(--ink);
  box-shadow: var(--shadow-card);
  margin: 8px 0 6px;
}
.live-url .lock { color: var(--good); font-size: 14px; }
.live-own { font-size: 14px; color: var(--ink-faint); margin: 0 0 32px; }
.live-actions { display: flex; flex-direction: column; align-items: center; gap: 14px; }

/* ---- clarify: confidence header -------------------------------------- */
.conf {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 16px 18px;
  margin-bottom: 30px;
  box-shadow: var(--shadow-card);
  animation: fade .6s var(--ease) both;
}
.conf-top {
  display: flex; align-items: baseline; justify-content: space-between;
  margin-bottom: 11px;
}
.conf-read { font-size: 15px; font-weight: 600; color: var(--ink); }
.conf-count { font-size: 13px; color: var(--ink-faint); }
.conf-bar {
  height: 7px; border-radius: 999px;
  background: var(--line); overflow: hidden;
}
.conf-fill {
  height: 100%; border-radius: 999px;
  background: linear-gradient(90deg, var(--accent), var(--accent-ink));
  /* The climb is the feature — make it visibly, calmly grow. */
  transition: width .9s var(--ease);
}
.conf-meta {
  display: flex; align-items: baseline; justify-content: space-between;
  margin-top: 9px;
}
.conf-pct { font-size: 13.5px; font-weight: 600; color: var(--accent-ink); }
.conf-note { font-size: 12.5px; color: var(--ink-faint); }

/* ---- clarify: the product-level suggestion (warmer than a question) -- */
.bubble-eng.suggestion {
  background: var(--accent-wash);
  border: 1px solid #efd9d0;
  border-radius: var(--r-md);
  padding: 18px 20px;
  max-width: 96%;
}
.suggestion-flag {
  display: inline-block;
  font-size: 11.5px; letter-spacing: .08em; text-transform: uppercase;
  font-weight: 700; color: var(--accent-ink);
  background: var(--surface); border: 1px solid rgba(202,138,4,0.30);
  border-radius: 999px; padding: 4px 11px; margin-bottom: 12px;
}

/* the always-available bypass under the dock */
.bypass-row { margin-top: 16px; text-align: center; }
.bypass-row .btn-quiet {
  font-size: 14px; color: var(--ink-faint);
  border: 1px dashed var(--line); border-radius: 999px; padding: 9px 18px;
}
.bypass-row .btn-quiet:hover { color: var(--accent-ink); border-color: var(--accent); }

/* ---- build: the mid-build question ----------------------------------- */
.midq {
  margin-top: 26px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-left: 3px solid var(--accent);
  border-radius: var(--r-md);
  padding: 20px 22px;
  box-shadow: var(--shadow-pop);
  animation: rise .55s var(--ease) both;
}
.midq .q-text { font-size: 17.5px; margin-bottom: 6px; }
.midq .dock-chips { margin-top: 14px; }
.midq .dock-row { margin-top: 12px; }
.midq.settled {
  display: flex; align-items: center; gap: 14px;
  border-left-color: var(--good);
}
.midq-answer {
  font-size: 15.5px; font-weight: 560; color: var(--ink);
  background: var(--accent-wash); color: var(--accent-ink);
  border-radius: 999px; padding: 8px 16px;
}
.midq-resume { font-size: 14px; color: var(--ink-soft); }

/* ---- QA: split preview + checklist ----------------------------------- */
.qa-grid {
  display: grid;
  grid-template-columns: minmax(280px, 340px) 1fr;
  gap: 44px;
  align-items: start;
  margin-top: 10px;
}
.qa-preview { display: flex; flex-direction: column; align-items: center; gap: 14px; }

/* a small, believable phone frame holding the generated app */
.phone {
  width: 300px;
  background: #15140f;
  border-radius: 36px;
  padding: 10px;
  box-shadow: var(--shadow-pop);
  position: relative;
}
.phone-notch {
  position: absolute; top: 16px; left: 50%; transform: translateX(-50%);
  width: 86px; height: 6px; border-radius: 999px; background: #2c2a24; z-index: 2;
}
.phone-screen {
  background: var(--paper);
  border-radius: 28px;
  overflow: hidden;
  height: 540px;
  display: flex; flex-direction: column;
}
.app-bar {
  display: flex; align-items: center; justify-content: space-between;
  padding: 30px 18px 14px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
}
.app-bar-title { font-size: 16px; font-weight: 680; letter-spacing: -.01em; }
.app-bar-dot { width: 26px; height: 26px; border-radius: 50%; background: var(--accent-wash); }
.app-body { padding: 16px 14px; overflow-y: auto; flex: 1; }
.app-h { font-size: 13px; font-weight: 600; color: var(--ink-faint); text-transform: uppercase; letter-spacing: .08em; margin: 0 0 12px; }
.app-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 13px 14px;
  margin-bottom: 11px;
  box-shadow: var(--shadow-card);
  animation: slideIn .5s var(--ease) both;
}
.app-card-top { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 5px; }
.app-card-title { font-size: 14.5px; font-weight: 600; }
.app-card-price { font-size: 14px; color: var(--accent-ink); font-weight: 620; }
.app-card-meta { font-size: 12.5px; color: var(--ink-soft); display: flex; gap: 7px; align-items: center; margin-bottom: 11px; }
.app-card-meta .dotsep { color: var(--ink-faint); }
.app-cta {
  font: inherit; font-size: 13.5px; font-weight: 560; color: #fff;
  background: var(--accent); border: none; border-radius: 9px;
  padding: 9px 14px; width: 100%; cursor: pointer;
  transition: background .25s var(--ease);
}
.app-cta:hover { background: var(--accent-ink); }
.app-cta.waitlist { background: var(--surface); color: var(--accent-ink); border: 1px solid var(--accent); }
.qa-draft-tag { font-size: 13px; color: var(--ink-faint); text-align: center; margin: 0; }

.qa-side { padding-top: 4px; }
.qa-checks { display: flex; flex-direction: column; gap: 10px; margin: 6px 0 14px; }
.qa-check {
  display: flex; align-items: center; gap: 13px;
  font: inherit; font-size: 15.5px; color: var(--ink); text-align: left;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 13px 15px; cursor: pointer; width: 100%;
  transition: all .25s var(--ease);
}
.qa-check:hover { border-color: var(--accent); transform: translateY(-1px); }
.qa-check.on { background: rgba(22,163,74,0.12); border-color: rgba(22,163,74,0.35); }
.qa-tick {
  flex: 0 0 auto;
  width: 22px; height: 22px; border-radius: 7px;
  border: 1.5px solid var(--line);
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 13px; color: #fff;
  transition: all .25s var(--ease);
}
.qa-check.on .qa-tick { background: var(--good); border-color: var(--good); }
.qa-check-text { line-height: 1.4; }
.qa-progress { font-size: 13.5px; color: var(--ink-faint); margin: 0; }

/* ---- bug form -------------------------------------------------------- */
.bug-field { margin-bottom: 24px; }
.bug-field .section-hint { margin-bottom: 9px; }
.bug-gate { font-size: 13.5px; color: var(--ink-faint); margin-top: 16px; }
.btn-primary:disabled {
  background: var(--line); color: var(--ink-faint);
  box-shadow: none; cursor: not-allowed; transform: none;
}
.btn-primary:disabled:hover { background: var(--line); transform: none; box-shadow: none; }

/* ---- live: publishing beat ------------------------------------------- */
.publishing { padding-top: 40px; animation: fade .5s var(--ease) both; }
.publishing .h1 { margin-top: 22px; }
.spinner.big {
  width: 38px; height: 38px; border-width: 3px;
  margin: 0 auto;
}

/* ── Stories panel (the editable source of truth on the refinement screen) ── */
.stories-panel {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 18px 20px;
  margin-bottom: 28px;
  box-shadow: var(--shadow-card);
}
.stories-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
}
.stories-status {
  font-size: 12px;
  color: var(--ink-faint);
  white-space: nowrap;
}
.stories-list { margin-top: 14px; }
.story-card {
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 14px 16px;
  transition: border-color .25s var(--ease), box-shadow .25s var(--ease);
}
.story-card + .story-card { margin-top: 10px; }
.story-card:hover { box-shadow: var(--shadow-card); }
.story-card-head {
  display: flex;
  align-items: baseline;
  gap: 10px;
}
.story-title { font-weight: 600; color: var(--ink); }
.story-for { font-size: 13px; color: var(--ink-soft); flex: 1; }
.story-edit, .story-remove {
  font: inherit;
  font-size: 14px;
  line-height: 1;
  border: none;
  background: transparent;
  color: var(--ink-faint);
  cursor: pointer;
  padding: 4px 6px;
  border-radius: var(--r-sm);
  transition: color .2s var(--ease), background .2s var(--ease);
}
.story-edit:hover { color: var(--accent); background: var(--accent-wash); }
.story-remove:hover { color: var(--accent-ink); background: var(--accent-wash); }
.story-wants {
  margin: 8px 0 0;
  padding-left: 18px;
  color: var(--ink-soft);
  font-size: 14px;
}
.story-wants li { margin: 2px 0; }
.story-sothat {
  margin: 8px 0 0;
  font-size: 13px;
  color: var(--accent-ink);
}
.add-story { margin-top: 14px; }

/* Refinement controls: review button + the shared-design opt-ins */
.refine-controls {
  margin-top: 18px;
  padding-top: 16px;
  border-top: 1px solid var(--line-soft);
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.review-btn { align-self: flex-start; }
.opt-in {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  font-size: 14px;
  color: var(--ink-soft);
  cursor: pointer;
}
.opt-in input { margin-top: 3px; accent-color: var(--accent); }
.historical-note {
  font-size: 13px;
  color: var(--accent-ink);
  background: var(--accent-wash);
  padding: 8px 12px;
  border-radius: var(--r-sm);
  margin: 0;
}

/* ---- live: maintenance panel ------------------------------------------ */
/* Calm, secondary. Sits below the live actions and never competes with them. */
.maintenance-panel {
  margin-top: 40px;
  padding: 20px 22px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  box-shadow: var(--shadow-card);
  text-align: left;
  animation: rise .8s var(--ease) both;
  animation-delay: .25s;
}
.maintenance-header {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
}
.maintenance-icon {
  font-size: 14px;
  color: var(--good);
  line-height: 1;
}
.maintenance-title {
  font-size: 13px;
  font-weight: 600;
  letter-spacing: .01em;
  color: var(--ink-soft);
}
.maintenance-note {
  font-size: 14.5px;
  line-height: 1.55;
  color: var(--ink-soft);
  margin: 0 0 14px;
}
.maintenance-note:last-child { margin-bottom: 0; }
.maintenance-confirmed {
  color: var(--good);
}
.maintenance-update-btn {
  font: inherit;
  font-size: 14px;
  font-weight: 560;
  color: var(--accent-ink);
  background: var(--accent-wash);
  border: 1px solid #efd9d0;
  border-radius: 999px;
  padding: 9px 20px;
  cursor: pointer;
  transition: background .25s var(--ease), transform .2s var(--ease);
}
.maintenance-update-btn:hover {
  background: rgba(176,67,46,0.20);
  transform: translateY(-1px);
}
.maintenance-update-btn:active { transform: translateY(0); }

/* Intake style picker: palette swatches + selectable chips */
.chip.selected {
  border-color: var(--accent);
  background: var(--accent-wash);
  color: var(--accent-ink);
}
.swatch-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
  gap: 12px;
  margin-bottom: 8px;
}
.swatch {
  font: inherit;
  text-align: left;
  background: var(--surface);
  border: 1.5px solid var(--line);
  border-radius: var(--r-md);
  padding: 12px;
  cursor: pointer;
  display: flex;
  flex-direction: column;
  gap: 6px;
  transition: border-color .2s var(--ease), box-shadow .2s var(--ease);
}
.swatch:hover { box-shadow: var(--shadow-card); }
.swatch.selected { border-color: var(--accent); box-shadow: var(--shadow-card); }
.swatch-chips { display: flex; gap: 5px; }
.swatch-chip {
  width: 22px; height: 22px; border-radius: 6px; display: inline-block;
}
.swatch-name { font-weight: 600; font-size: 14px; color: var(--ink); }
.swatch-desc { font-size: 12px; color: var(--ink-soft); line-height: 1.35; }

/* Persistence banner — shown only when version-history durability is degraded.
   Calm and informative, not alarming: a soft amber wash, a small dot, plain text.
   Sits above the rail so it reads as ambient status, not an error modal. */
.persist-banner {
  display: flex; align-items: center; gap: 10px;
  padding: 9px 18px;
  background: rgba(202,138,4,0.10);    /* dark amber wash */
  border-bottom: 1px solid rgba(202,138,4,0.3);
  color: #fbbf24;
  font-size: 13px; line-height: 1.4;
}
.persist-banner-dot {
  flex: none; width: 8px; height: 8px; border-radius: 50%;
  background: #d9a441;                 /* amber dot — noticeable, not urgent */
}
.persist-banner-text { font-weight: 500; }

/* ===================================================================== */
/* Edition switcher — flip between the two surfaces in one window.        */
/* ===================================================================== */
.edition-switcher {
  display: flex; align-items: center; gap: 16px;
  padding: 8px 16px;
  background: var(--ink);
  color: #f4f1ea;
  /* Full width so app-root's align-items:center can't shrink it to content
     width (which differs per view and shifted the toolbar). */
  width: 100%;
}
.edition-brand { font-weight: 700; letter-spacing: .02em; font-size: 14px; }
.edition-tabs { display: flex; gap: 4px; background: #00000033; border-radius: 9px; padding: 3px; }
.edition-tab {
  border: none; background: transparent; color: #d8d3c8;
  font-size: 13px; font-weight: 600; padding: 6px 14px; border-radius: 7px;
  cursor: pointer; transition: background .15s var(--ease), color .15s var(--ease);
}
.edition-tab:hover { color: #fff; }
.edition-tab.on { background: var(--accent); color: #fff; }
.edition-hint { margin-left: auto; font-size: 12px; color: #b7b1a5; font-style: italic; }

/* Start over — a quiet reset on the right of the consumer rail. */
.rail-row { display: flex; align-items: center; }
.rail-row .rail { flex: 1; }
.btn-restart {
  flex: none; margin-right: 18px;
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft);
  font-size: 12.5px; font-weight: 600; padding: 6px 13px; border-radius: 8px;
  cursor: pointer; transition: border-color .15s var(--ease), color .15s var(--ease);
}
.btn-restart:hover { border-color: var(--accent); color: var(--accent-ink); }

/* ===================================================================== */
/* The enterprise cockpit — a dense, single-pane control surface.        */
/* ===================================================================== */
.cockpit { display: flex; flex-direction: column; flex: 1; min-height: 0; width: 100%; background: transparent; }

/* Top bar */
.cockpit-topbar { padding: 10px 16px; border-bottom: 1px solid var(--line); background: var(--surface); }
.topbar-line1 { display: flex; align-items: center; gap: 12px; }
.topbar-brand { font-weight: 700; font-size: 13px; color: var(--ink); }
.topbar-story { font-size: 13px; color: var(--ink-soft); }
.topbar-status { margin-left: auto; font-size: 11px; font-weight: 700; letter-spacing: .04em; padding: 3px 9px; border-radius: 6px; }
/* Toast notifications (top-right overlay). */
/* A separate top-layer overlay: fixed to the viewport, above everything, and
   click-through (pointer-events:none) except on the toasts themselves. */
.toast-host { position: fixed; top: 14px; right: 14px; z-index: 2147483000; display: flex; flex-direction: column; gap: 8px; width: 360px; max-width: calc(100vw - 28px); pointer-events: none; }
.toast { pointer-events: auto; display: flex; align-items: flex-start; gap: 8px; padding: 10px 12px; border-radius: 10px; border: 1px solid var(--line); background: var(--surface); box-shadow: 0 8px 28px rgba(0,0,0,.18); font-size: 12px; line-height: 1.45; animation: toast-in .18s ease-out; }
@keyframes toast-in { from { opacity: 0; transform: translateY(-6px); } to { opacity: 1; transform: none; } }
.toast-label { font-weight: 700; font-size: 10px; letter-spacing: .06em; padding: 2px 6px; border-radius: 5px; flex: none; margin-top: 1px; }
.toast-msg { color: var(--ink); flex: 1; }
.toast-close { background: none; border: none; font-size: 17px; line-height: 1; color: var(--ink-faint); cursor: pointer; flex: none; width: 20px; height: 20px; border-radius: 5px; display: flex; align-items: center; justify-content: center; margin: -1px -3px 0 0; }
.toast-close:hover { background: rgba(0,0,0,.08); color: var(--ink); }
.toast.info { border-color: rgba(47,95,158,0.40); }
.toast.info .toast-label { background: rgba(47,95,158,0.20); color: #7ca8e0; }
.toast.warning { border-color: rgba(202,138,4,0.5); background: rgba(202,138,4,0.10); }
.toast.warning .toast-label { background: rgba(202,138,4,0.22); color: #fbbf24; }
.toast.error { border-color: rgba(220,38,38,0.45); background: rgba(220,38,38,0.10); }
.toast.error .toast-label { background: rgba(220,38,38,0.22); color: #f87171; }

/* Stage tabs: now clickable buttons, not inert spans. */
.stage-tab { cursor: pointer; background: none; border: none; font: inherit; }
.stage-tab.view { box-shadow: inset 0 -2px 0 var(--accent); }
.fleet-idle { font-size: 12px; color: var(--ink-faint); }
.gate-tally.idle { color: var(--ink-faint); }
.stage-name { font-size: 12px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; color: var(--accent); margin: 2px 0 8px; }
.stage-not-reached { font-size: 12px; color: var(--ink-faint); margin-top: 10px; font-style: italic; }
.stage-not-reached-now { font-weight: 700; font-style: normal; }

/* Onboard view. */
/* Wide enough for the grouped rules + findings tables to breathe; prose blocks below
   cap their own line length for readability. */
.onboard { max-width: 1180px; margin: 0 auto; padding: 28px 24px; }
.onboard-sub, .scan-section-sub, .scan-domains-note { max-width: 90ch; }
.onboard-head { margin-bottom: 18px; }
.onboard-title { font-size: 19px; font-weight: 700; color: var(--ink); }
.onboard-sub { font-size: 13px; color: var(--ink-soft); margin-top: 5px; line-height: 1.5; }
.onboard-paths { display: flex; gap: 12px; margin-bottom: 16px; }
.onboard-path { flex: 1; text-align: left; cursor: pointer; background: var(--surface); border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; display: flex; flex-direction: column; gap: 3px; }
.onboard-path.on { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
.onboard-path-h { font-weight: 700; font-size: 13px; color: var(--ink); }
.onboard-path-d { font-size: 12px; color: var(--ink-soft); }
.onboard-gate { display: flex; gap: 10px; align-items: flex-start; background: rgba(202,138,4,0.10); border: 1px solid rgba(202,138,4,0.4); border-radius: 10px; padding: 12px 14px; margin-bottom: 16px; }
.onboard-gate-dot { width: 8px; height: 8px; border-radius: 50%; background: #ca8a04; margin-top: 5px; flex: none; }
.onboard-gate-h { font-weight: 700; font-size: 13px; color: #fbbf24; }
.onboard-gate-b { font-size: 12px; color: var(--ink-soft); margin-top: 3px; line-height: 1.5; }
.mono { font-family: "Courier Prime", ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .92em; }
.onboard-repo-block { display: flex; flex-direction: column; gap: 6px; margin-bottom: 22px; }
.onboard-repo-label { font-size: 12px; color: var(--ink-soft); }
.onboard-repos-input { width: 100%; box-sizing: border-box; padding: 9px 11px; border: 1px solid var(--line); border-radius: 8px; font: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; resize: vertical; }

/* Non-blocking onboarding note (replaces the old connect-GitHub gate). */
.onboard-note { font-size: 12px; color: var(--ink-soft); background: var(--surface); border: 1px solid var(--line); border-radius: 8px; padding: 9px 12px; margin-bottom: 16px; line-height: 1.5; }

/* Added-repos list (browse-only onboarding): one chip per local repo, with a remove ×. */
.onboard-repos-empty { font-size: 13px; color: var(--ink-faint); padding: 9px 11px; border: 1px dashed var(--line); border-radius: 8px; }
.onboard-repos-list { display: flex; flex-direction: column; gap: 4px; }
.onboard-repo-chip { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 7px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--paper); }
.onboard-repo-chip-name { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; color: var(--ink); }
.onboard-repo-chip-x { border: none; background: transparent; color: var(--ink-faint); font-size: 13px; line-height: 1; cursor: pointer; padding: 2px 5px; border-radius: 5px; transition: background .12s var(--ease), color .12s var(--ease); }
.onboard-repo-chip-x:hover { background: rgba(176,67,46,0.22); color: var(--ink); }

/* Custom rules panel (#49): author free-text rules that appear in the Custom / Custom Global
   table groups. Sits above the proposed-rules table. */
.custom-rules { border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; margin: 12px 0 16px; background: var(--surface); }
.custom-rules-head { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.custom-rules-title { font-weight: 700; font-size: 13px; color: var(--ink); margin-right: auto; }
.custom-rules-sub { font-size: 12px; color: var(--ink-soft); margin: 6px 0 0; line-height: 1.5; }
.custom-rules-empty { font-size: 12px; color: var(--ink-faint); margin: 8px 0 0; }
.custom-rules-list { display: flex; flex-direction: column; gap: 4px; margin-top: 8px; }
.custom-rule-row { display: flex; align-items: center; gap: 8px; padding: 6px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--paper); }
.custom-rule-name { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; color: var(--ink); }
.custom-rule-scope { font-size: 11px; color: var(--ink-soft); background: var(--accent-wash); border-radius: 5px; padding: 2px 7px; }
/* Push the Edit/Delete pair to the right; they share the row's 8px gap. */
.custom-rule-row .btn-edit-sm { margin-left: auto; }
.custom-rule-editor { display: flex; flex-direction: column; gap: 8px; margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--line); }
.custom-rule-editor-actions { display: flex; align-items: center; gap: 8px; }
.custom-rule-editor-actions .custom-rule-scope { margin-right: auto; }
.custom-rule-editor-actions .btn-run { margin-bottom: 0; }
.onboard-cta {
  align-self: flex-start;
  padding: 9px 16px; border-radius: 8px; border: none;
  background: var(--accent); color: #fff; font-weight: 700; font-size: 13px; cursor: pointer;
  transition: background .15s var(--ease);
}
.onboard-cta:hover:not(:disabled) { background: var(--accent-ink); }
.onboard-cta:disabled { background: var(--line); color: var(--ink-faint); cursor: not-allowed; }
.onboard-steps { display: flex; flex-direction: column; gap: 12px; }
.onboard-step { display: flex; gap: 12px; align-items: flex-start; }
.onboard-step-n { width: 22px; height: 22px; flex: none; border-radius: 50%; background: var(--surface); border: 1px solid var(--line); color: var(--ink-soft); font-size: 12px; font-weight: 700; display: flex; align-items: center; justify-content: center; }
.onboard-step-h { font-weight: 700; font-size: 13px; color: var(--ink); }
.onboard-step-b { font-size: 12px; color: var(--ink-soft); margin-top: 2px; line-height: 1.5; }
.scan-results { margin-top: 18px; }
.scan-note { font-size: 12px; color: #fbbf24; background: rgba(202,138,4,0.10); border: 1px solid rgba(202,138,4,0.4); border-radius: 8px; padding: 8px 11px; margin-bottom: 12px; }
.scan-summary { display: flex; gap: 18px; margin-bottom: 16px; }
.scan-stat { font-size: 12px; color: var(--ink-soft); }
.scan-stat-n { font-size: 16px; font-weight: 700; color: var(--ink); margin-right: 4px; }
.scan-stat-n.high { color: #9a3526; }
.scan-section-h { font-size: 13px; font-weight: 700; color: var(--ink); margin: 18px 0 2px; }
.scan-section-sub { font-size: 12px; color: var(--ink-soft); margin-bottom: 8px; line-height: 1.5; }
.scan-stacks { display: flex; flex-direction: column; gap: 4px; margin: 12px 0; }
.scan-stack { display: flex; gap: 10px; align-items: baseline; font-size: 12px; }
.scan-stack-repo { font-weight: 700; color: var(--ink); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.scan-stack-tech { color: var(--ink-soft); }
.findings-toolbar { display: flex; gap: 8px; margin-bottom: 8px; }

/* Critical (security) row highlight: applied to the whole <tr> via chorale 0.2.3's
   row_class hook (replaces the old per-cell stripe). A red left border + faint red tint
   marks the security-floor rows unmistakably; selection background still composes over it. */
.finding-row-critical > td:first-child { box-shadow: inset 4px 0 0 0 #d4332b; }
.finding-row-critical > td { background: rgba(212, 51, 43, 0.06); }

/* Needs-a-choice row highlight (proposed-rules table): a SELECTED rule with alternatives but
   no chosen option. Amber tint + left border marks the rows that block audit/arm until the
   architect picks an alternative; clears once a choice is made (row reverts to normal blue).
   The class is only applied to selected rows, so this must out-specify chorale's selected-row
   rule (`.chorale-root tr[data-chorale-row-selected="true"] > td`, specificity 0,2,2) — the
   extra attribute selector here makes it 0,3,2 so the amber wins over the blue. */
.chorale-root tr.rule-row-needs-choice[data-chorale-row-selected="true"] > td {
  background: rgba(217, 158, 22, 0.18);
}
.chorale-root tr.rule-row-needs-choice[data-chorale-row-selected="true"] > td:first-child {
  box-shadow: inset 4px 0 0 0 #d99e16;
}

/* Why-are-the-buttons-disabled banner: sits under the proposed-rules table, above the
   audit/apply buttons. Amber to match the needs-a-choice row highlight. */
.rule-gate-warning {
  display: flex; align-items: flex-start; gap: 8px;
  margin: 0 0 8px; padding: 9px 12px;
  background: rgba(217, 158, 22, 0.10); border: 1px solid rgba(217, 158, 22, 0.45);
  border-radius: var(--r-sm); color: var(--ink-soft); font-size: 13px; line-height: 1.4;
}
.rule-gate-warning-icon { color: #b8860b; font-size: 14px; line-height: 1.3; flex-shrink: 0; }
.rule-gate-warning strong { color: var(--ink); font-weight: 600; }

/* Key above the findings table: what the stripe means. */
.findings-key { display: flex; flex-wrap: wrap; gap: 16px; margin: 4px 0 10px; }
.findings-key-item {
  display: inline-flex; align-items: center; gap: 7px;
  font-size: 12px; color: var(--ink-soft);
}
.findings-key-swatch { width: 14px; height: 14px; border-radius: 3px; flex: none; }
.findings-key-swatch.crit { background: #d4332b; }
.findings-key-swatch.arch { background: var(--surface); border: 1px solid var(--line); }
.alts { margin-top: 22px; }
.alts-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.alt-row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 10px 0; border-bottom: 1px solid var(--line); }
.alt-row.must { background: rgba(202,138,4,0.10); border-radius: 8px; padding: 10px 12px; border-bottom: none; margin: 4px 0; }
.alt-rule { display: flex; flex-direction: column; gap: 2px; }
.alt-rule-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; font-weight: 700; color: var(--ink); }
.alt-rule-title { font-size: 12px; color: var(--ink-soft); }
.alt-must { display: inline-block; margin-top: 3px; font-size: 10px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; color: #8a4f1d; }
.alt-select { min-width: 280px; max-width: 420px; padding: 7px 9px; border: 1px solid var(--line); border-radius: 8px; font: inherit; font-size: 12px; background: var(--surface); }
.alt-repos { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; margin-top: 6px; width: 100%; }
.alt-repos-label { font-size: 11px; color: var(--ink-faint); }
.repo-chip { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; padding: 3px 8px; border-radius: 999px; border: 1px solid var(--line); background: var(--surface); color: var(--ink-faint); cursor: pointer; }
.repo-chip.on { border-color: var(--accent); background: var(--accent); color: #fff; }
.proj-bar { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; margin: 14px 0 20px; }
.proj-label { font-size: 12px; color: var(--ink-soft); }
.proj-none { font-size: 12px; color: var(--ink-faint); font-style: italic; }
.proj-chip { font-size: 12px; padding: 5px 12px; border-radius: 999px; border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft); cursor: pointer; }
.proj-chip.on { border-color: var(--accent); background: var(--accent); color: #fff; }
.rules-sections { display: flex; gap: 14px; flex-wrap: wrap; margin: 12px 0 16px; }
.rules-emit { display: flex; align-items: center; gap: 12px; margin-bottom: 22px; flex-wrap: wrap; }
.rules-emit-hint { font-size: 12px; color: var(--ink-soft); }
.emit-toggle { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--ink-soft); cursor: pointer; user-select: none; }
.emit-toggle.disabled { opacity: 0.45; cursor: not-allowed; }
.emit-toggle input { cursor: inherit; }
.repo-multiselect { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; margin: 8px 0 6px; }
.repo-multiselect-label { font-size: 12px; font-weight: 700; color: var(--ink-soft); }
.repo-multiselect-hint { font-size: 11px; color: var(--ink-faint); font-style: italic; }
.rule-count { display: flex; flex-direction: column; min-width: 150px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
.rule-count-n { font-size: 22px; font-weight: 700; color: var(--ink); }
.rule-count-l { font-size: 12px; color: var(--ink-soft); }
.applied-list { display: flex; flex-direction: column; gap: 10px; margin-top: 12px; }
.applied-rule { border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; background: var(--surface); }
.applied-rule-head { display: flex; align-items: center; gap: 10px; }
.applied-rule-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; font-weight: 700; color: var(--ink); }
.applied-rule-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; color: var(--ink-faint); }
.applied-tag { font-size: 10px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; padding: 2px 6px; border-radius: 5px; }
.applied-tag.custom { background: rgba(47,95,158,0.18); color: #7ca8e0; }
.applied-tag.drift { background: rgba(220,38,38,0.15); color: #f87171; }
.applied-rule-title { font-size: 13px; color: var(--ink); margin-top: 5px; }
.applied-rule-summary { font-size: 12px; color: var(--ink-soft); margin-top: 4px; line-height: 1.5; }
.applied-options { display: flex; flex-direction: column; gap: 3px; margin-top: 8px; }
.applied-option { display: flex; gap: 8px; font-size: 12px; color: var(--ink-faint); padding: 3px 6px; border-radius: 6px; }
.applied-option.chosen { color: var(--ink); background: #eef6f0; }
.applied-option-mark { min-width: 56px; font-size: 11px; }
.applied-option-label { color: inherit; }
.topbar-line3 { display: flex; align-items: center; gap: 7px; margin-top: 5px; font-size: 12px; color: var(--ink-soft); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.topbar-axis-label { color: var(--ink-faint); }
.topbar-axis-val { color: var(--ink); }
.topbar-line2 { display: flex; align-items: center; gap: 9px; margin-top: 6px; font-size: 12px; color: var(--ink-soft); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.topbar-meter { color: var(--ink); }
.meter-est { color: var(--ink-faint); }
.topbar-sep { color: var(--ink-faint); }
.conn-ok { color: #2f8f5b; }
.conn-warn { color: #b06a2e; }

/* Three-pane body */
.cockpit-body { flex: 1; display: grid; grid-template-columns: 250px 1fr 290px; min-height: 0; }
.cockpit-rail, .cockpit-inspector { padding: 14px; overflow-y: auto; }
.cockpit-rail { border-right: 1px solid var(--line); background: var(--rail-tint); }
.cockpit-inspector { border-left: 1px solid var(--line); background: var(--rail-tint); }
.cockpit-rail-label { font-size: 11px; font-weight: 700; letter-spacing: .06em; color: var(--ink-faint); margin: 0 0 8px; }
.cockpit-rail-label.needs { margin-top: 20px; color: var(--accent-ink); }

/* Story spine */
.spine-list { display: flex; flex-direction: column; gap: 6px; }
.spine-item {
  display: flex; flex-direction: column; gap: 6px; align-items: flex-start;
  text-align: left; border: 1px solid var(--line); background: var(--surface);
  border-radius: 9px; padding: 9px 10px; cursor: pointer;
  transition: border-color .15s var(--ease), box-shadow .15s var(--ease);
}
.spine-item:hover { border-color: var(--ink-faint); }
.spine-item.sel { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-wash); }
.spine-title { font-size: 13px; font-weight: 600; color: var(--ink); line-height: 1.3; }
.spine-badge { font-size: 10px; font-weight: 700; letter-spacing: .04em; padding: 2px 7px; border-radius: 5px; }
.spine-new {
  margin-top: 4px; border: 1px dashed var(--line); background: transparent;
  color: var(--ink-faint); font-size: 12.5px; padding: 8px; border-radius: 9px; cursor: pointer;
}
.spine-new:hover { color: var(--accent-ink); border-color: var(--accent); }

/* Status badges (shared by spine + topbar) */
.spine-badge.neutral, .topbar-status.neutral { background: rgba(140,128,117,0.18); color: var(--ink-soft); }
.spine-badge.active,  .topbar-status.active  { background: rgba(47,95,158,0.22); color: #7ca8e0; }
.spine-badge.warn,    .topbar-status.warn    { background: rgba(202,138,4,0.18); color: #fbbf24; }
.spine-badge.done,    .topbar-status.done    { background: rgba(22,163,74,0.18); color: #4ade80; }
.spine-badge.block,   .topbar-status.block   { background: rgba(176,67,46,0.20); color: #f87171; }

/* UoW dev-status badge — compact pill shown alongside the tracker status badge in the spine.
   New=gray, In progress=accent (terracotta), Done=green. Visually distinct from spine-badge
   (lighter weight, italicized label) so the two statuses read as orthogonal layers. */
.uow-dev-badge {
  font-size: 9px; font-weight: 700; font-style: italic; letter-spacing: .04em;
  padding: 2px 6px; border-radius: 5px; text-transform: uppercase;
  border: 1px solid transparent; flex-shrink: 0;
}
.uow-dev-badge.neutral { background: #ece9e3; color: #6c6862; border-color: #dedad3; }
.uow-dev-badge.accent  { background: var(--accent-wash); color: var(--accent-ink); border-color: #e5c9bd; }
.uow-dev-badge.green   { background: #e2f1e7; color: #2f8f5b; border-color: #c5e3ce; }

/* UoW panel — appears in the center stage below the run + agent-activity blocks. */
.uow-panel {
  margin-top: 16px; border: 1px solid var(--line); border-radius: 10px;
  padding: 14px 16px; background: var(--surface);
}
.uow-panel-h {
  font-size: 10px; font-weight: 800; letter-spacing: .07em; text-transform: uppercase;
  color: var(--ink-faint); margin: 0 0 12px;
}
.uow-status-row, .uow-branch-row {
  display: flex; align-items: center; gap: 10px; margin-bottom: 10px;
}
.uow-field-label {
  font-size: 11.5px; font-weight: 600; color: var(--ink-soft); min-width: 76px;
}
/* Segmented control for the 3-state dev-status selector. A genuinely distinct pattern
   (one control, mutually-exclusive options), but aligned to the shared button system:
   the system radius (8px), the secondary's small font/weight, and the primary accent for
   the active segment — so it reads as the same family, not a one-off. */
.uow-seg {
  display: inline-flex; border: 1px solid var(--line); border-radius: 8px; overflow: hidden;
  background: var(--paper);
}
.uow-seg-btn {
  font-size: 12px; font-weight: 600; padding: 5px 11px;
  border: none; border-right: 1px solid var(--line); background: transparent;
  color: var(--ink-soft); cursor: pointer; transition: background .12s var(--ease), color .12s var(--ease);
}
.uow-seg-btn:last-child { border-right: none; }
.uow-seg-btn:hover { background: var(--line-soft); color: var(--ink); }
.uow-seg-btn.active { background: var(--accent); color: #fff; font-weight: 700; }

.uow-branch-val {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px; color: var(--ink); background: var(--accent-wash);
  padding: 2px 8px; border-radius: 5px; border: 1px solid #e5c9bd;
}
.uow-branch-none { font-size: 12px; color: var(--ink-faint); font-style: italic; }

/* AI history list */
.uow-history { margin-top: 6px; }
.uow-history-h {
  font-size: 10px; font-weight: 800; letter-spacing: .07em; text-transform: uppercase;
  color: var(--ink-faint); margin: 0 0 6px;
}
.uow-history-empty { font-size: 12px; color: var(--ink-faint); font-style: italic; margin: 0; }
.uow-history-list { display: flex; flex-direction: column; gap: 4px; }
.uow-history-row {
  display: grid; grid-template-columns: auto 80px 1fr; gap: 8px; align-items: baseline;
  font-size: 11.5px; padding: 5px 8px; border-radius: 6px;
  background: var(--paper); border: 1px solid var(--line-soft);
}
.uow-hist-ts {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 10.5px; color: var(--ink-faint); white-space: nowrap;
}
.uow-hist-kind {
  font-size: 10px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase;
  color: var(--accent-ink); background: var(--accent-wash);
  padding: 1px 5px; border-radius: 4px; white-space: nowrap; text-align: center;
}
.uow-hist-text { color: var(--ink); line-height: 1.4; }

/* NEEDS YOU queue */
.needs-list { display: flex; flex-direction: column; gap: 6px; }
.needs-item {
  display: flex; align-items: flex-start; gap: 8px; text-align: left;
  border: 1px solid rgba(202,138,4,0.3); background: rgba(202,138,4,0.08); border-radius: 9px;
  padding: 9px 10px; cursor: pointer; font-size: 12.5px; color: var(--ink); line-height: 1.35;
}
.needs-item:hover { border-color: var(--accent); }
.needs-dot { flex: none; width: 8px; height: 8px; border-radius: 50%; margin-top: 4px; }
.needs-dot.warn { background: #d9a441; }
.needs-q { display: block; }
.needs-who { display: block; margin-top: 2px; font-size: 11px; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.needs-empty { font-size: 12.5px; color: var(--ink-faint); font-style: italic; margin: 0; }

/* Governed Development REVIEW card (a run paused at AwaitingReview: test-tamper etc.). Amber,
   like the clarification card, but with the three explicit decision actions. */
.uow-review-card {
  border: 1px solid rgba(202,138,4,0.45); background: rgba(202,138,4,0.10); border-radius: 10px;
  padding: 11px 12px; display: flex; flex-direction: column; gap: 8px;
}
.uow-review-head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.uow-review-badge {
  flex: none; font-size: 10px; font-weight: 800; letter-spacing: 0.04em; text-transform: uppercase;
  color: #7a5b00; background: rgba(202,138,4,0.22); border-radius: 6px; padding: 2px 7px;
}
.uow-review-rule { font-size: 12px; font-weight: 700; color: var(--ink); line-height: 1.35; }
.uow-review-stopped { font-size: 12.5px; color: var(--ink); margin: 0; line-height: 1.4; }
.uow-review-context {
  font-size: 11px; color: var(--ink-faint); margin: 0; line-height: 1.35;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; word-break: break-word;
}
.uow-review-suggestions { margin: 0; padding-left: 16px; display: flex; flex-direction: column; gap: 3px; }
.uow-review-suggestions li { font-size: 12px; color: var(--ink-faint); line-height: 1.35; }
.uow-review-input {
  width: 100%; box-sizing: border-box; resize: vertical; border-radius: 8px;
  border: 1px solid var(--line); padding: 7px 9px; font-size: 12.5px; color: var(--ink);
  background: rgba(255,255,255,0.7); font-family: inherit;
}
.uow-review-actions { display: flex; gap: 7px; flex-wrap: wrap; }
.uow-review-actions button { font-size: 12px; }
.uow-review-status { font-size: 11.5px; color: var(--ink-faint); font-style: italic; margin: 0; }
.uow-review-chat-log {
  display: flex; flex-direction: column; gap: 5px; max-height: 180px; overflow-y: auto;
  padding: 6px; border-radius: 8px; background: rgba(255,255,255,0.5);
}
.uow-review-msg { font-size: 12px; line-height: 1.4; padding: 5px 8px; border-radius: 7px; white-space: pre-wrap; }
.uow-review-msg.user { background: rgba(202,138,4,0.14); color: var(--ink); align-self: flex-end; max-width: 88%; }
.uow-review-msg.ai { background: rgba(255,255,255,0.85); color: var(--ink); align-self: flex-start; max-width: 88%; }
.uow-review-chat { display: flex; gap: 6px; align-items: flex-end; }
.uow-review-chat-input {
  flex: 1; box-sizing: border-box; resize: vertical; border-radius: 8px; border: 1px solid var(--line);
  padding: 6px 8px; font-size: 12px; color: var(--ink); background: rgba(255,255,255,0.7); font-family: inherit;
}
.uow-review-ask { flex: none; font-size: 12px; }

/* Soft-context settings editors (#112): product brief + operating principles + memory. Dark, to
   match the other per-project settings cards (.tier-map-editor et al.). */
.soft-ctx-card {
  border: 1px solid var(--line); border-radius: var(--r-md); padding: 14px 16px; margin: 8px 0;
  display: flex; flex-direction: column; gap: 9px; background: var(--surface);
}
.soft-ctx-title { font-size: 14px; font-weight: 600; color: var(--ink); margin: 0; }
.soft-ctx-sub { font-size: 12px; color: var(--ink-soft); margin: 0; line-height: 1.4; }
.soft-ctx-brief {
  width: 100%; box-sizing: border-box; resize: vertical; border-radius: 6px; border: 1px solid var(--line);
  padding: 8px 10px; font-size: 12.5px; color: var(--ink); background: #11100f;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; line-height: 1.45;
}
.op-list { display: flex; flex-direction: column; gap: 4px; max-height: 320px; overflow-y: auto; padding-right: 3px; }
.op-row {
  display: flex; align-items: flex-start; gap: 8px; padding: 6px 8px; border-radius: 7px;
  border: 1px solid var(--line); font-size: 12.5px; color: var(--ink-soft); line-height: 1.4; cursor: pointer;
}
.op-row.on { color: var(--ink); border-color: rgba(34,197,94,0.35); background: rgba(34,197,94,0.08); }
.op-row input { margin-top: 2px; flex: none; }
.op-text { display: block; }
.op-add { display: flex; gap: 6px; align-items: center; }
.op-add-input {
  flex: 1; box-sizing: border-box; border-radius: 6px; border: 1px solid var(--line);
  padding: 6px 9px; font-size: 12.5px; color: var(--ink); background: #11100f; font-family: inherit;
}
/* Work hierarchy (design-page) drag-and-drop schema builder. */
.hier-palette { display: flex; flex-wrap: wrap; gap: 6px; margin: 6px 0 4px; }
.hier-chip {
  display: inline-flex; align-items: center; gap: 5px; font-size: 12px; font-weight: 600;
  color: var(--ink); background: var(--line-soft); border: 1px solid var(--line);
  border-radius: 6px; padding: 3px 9px; user-select: none;
}
.hier-palette-chip { cursor: grab; }
.hier-palette-chip:active { cursor: grabbing; }
.hier-add { display: flex; gap: 8px; margin: 4px 0 10px; }
.hier-types { display: flex; flex-direction: column; gap: 8px; max-height: 360px; overflow-y: auto; padding-right: 3px; }
.hier-type-card { border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; background: #11100f; }
.hier-type-head { display: flex; align-items: center; gap: 10px; }
.hier-type-head .hier-chip { cursor: grab; }
.hier-root-toggle { display: inline-flex; align-items: center; gap: 4px; font-size: 11.5px; color: var(--ink-soft); margin-left: auto; }
.hier-remove, .hier-chip-x {
  border: none; background: transparent; color: var(--ink-faint); cursor: pointer;
  font-size: 12px; line-height: 1; padding: 0 2px;
}
.hier-remove:hover, .hier-chip-x:hover { color: #f87171; }
.hier-children {
  display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin-top: 8px;
  border: 1px dashed var(--line); border-radius: 6px; padding: 6px 8px; min-height: 30px;
}
.hier-children-label { font-size: 11px; color: var(--ink-faint); text-transform: uppercase; letter-spacing: 0.04em; }
.hier-child-chip { background: rgba(202,138,4,0.14); border-color: rgba(202,138,4,0.35); }
.hier-drop-hint { font-size: 11px; color: var(--ink-faint); font-style: italic; margin-left: auto; }
/* Project memory (Layer 3) curation list. */
.mem-badge {
  margin-left: 8px; font-size: 10.5px; font-weight: 700; color: #fbbf24;
  background: rgba(202,138,4,0.18); border-radius: 6px; padding: 1px 7px;
}
.mem-empty { font-style: italic; }
.mem-list { display: flex; flex-direction: column; gap: 5px; max-height: 300px; overflow-y: auto; padding-right: 3px; }
.mem-row {
  display: flex; align-items: flex-start; justify-content: space-between; gap: 8px;
  border: 1px solid var(--line); border-radius: 8px; padding: 7px 9px;
}
.mem-row.mem-proposed { border-color: rgba(202,138,4,0.45); background: rgba(202,138,4,0.10); }
.mem-row.mem-archived { opacity: 0.5; }
.mem-row-main { display: flex; flex-wrap: wrap; align-items: baseline; gap: 6px; flex: 1; min-width: 0; }
.mem-kind {
  flex: none; font-size: 9.5px; font-weight: 800; text-transform: uppercase; letter-spacing: 0.03em;
  color: var(--ink-faint); border: 1px solid var(--line); border-radius: 5px; padding: 1px 5px;
}
.mem-text {
  font-size: 12.5px; color: var(--ink); line-height: 1.4; word-break: break-word;
  display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;
}
.mem-clickable { cursor: pointer; border-radius: 6px; }
.mem-clickable:hover .mem-text { color: var(--accent); }
.mem-src { font-size: 10.5px; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.mem-actions { flex: none; display: flex; gap: 5px; }
.mem-btn {
  font-size: 11px; padding: 3px 9px; border-radius: 6px; border: 1px solid var(--line);
  background: var(--line-soft); color: var(--ink); cursor: pointer;
}
.mem-btn:hover { border-color: var(--accent); }
.mem-del:hover { border-color: #dc2626; color: #f87171; }
.mem-add { display: flex; gap: 6px; align-items: center; margin-top: 4px; }
.mem-kind-select {
  flex: none; border-radius: 6px; border: 1px solid var(--line); padding: 6px 8px;
  font-size: 12px; color: var(--ink); background: #11100f;
}
/* Project-memory view/edit modal (reuses .rule-modal-overlay + .rule-modal). */
.mem-edit-modal { max-width: 520px; display: flex; flex-direction: column; gap: 10px; }
.mem-edit-text {
  width: 100%; box-sizing: border-box; resize: vertical; border-radius: 6px; border: 1px solid var(--line);
  padding: 8px 10px; font-size: 13px; color: var(--ink); background: #11100f; line-height: 1.5;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
.mem-edit-actions { display: flex; gap: 8px; justify-content: flex-end; }
/* "+ Add to learnings" affordance on chat AI replies (#112). */
.chat-add-learning {
  align-self: flex-start; margin-top: 6px; background: none; border: none; padding: 0;
  font-size: 11px; color: #2563eb; cursor: pointer; opacity: 0.7;
}
.chat-add-learning:hover { opacity: 1; text-decoration: underline; }

/* AskUserQuestion-style structured clarification card (reusable). */
.clarify-q-card {
  border: 1px solid rgba(202,138,4,0.3); background: rgba(202,138,4,0.08); border-radius: 10px;
  padding: 11px 12px; display: flex; flex-direction: column; gap: 9px;
}
.clarify-q-question { font-size: 13px; font-weight: 700; color: var(--ink); margin: 0; line-height: 1.4; }
.clarify-q-addressee { font-size: 11px; color: var(--ink-faint); margin: -4px 0 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.clarify-q-options { display: flex; flex-direction: column; gap: 6px; }
.clarify-q-option {
  display: flex; align-items: flex-start; gap: 8px; cursor: pointer;
  border: 1px solid var(--line); background: var(--surface); border-radius: 8px; padding: 8px 9px;
}
.clarify-q-option:hover { border-color: var(--accent); }
.clarify-q-option.on { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-wash); }
.clarify-q-option input { margin-top: 2px; flex: none; }
.clarify-q-option-body { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.clarify-q-option-label { font-size: 12.5px; font-weight: 600; color: var(--ink); }
.clarify-q-option-desc { font-size: 11.5px; color: var(--ink-soft); line-height: 1.35; }
.clarify-q-other { display: flex; flex-direction: column; gap: 4px; }
.clarify-q-other-label { font-size: 11px; font-weight: 700; letter-spacing: .04em; color: var(--ink-faint); }
.clarify-q-other-input {
  font: inherit; font-size: 12.5px; border: 1px solid var(--line); border-radius: 8px;
  padding: 7px 9px; resize: vertical; background: var(--surface); color: var(--ink);
}
.clarify-q-other-input:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-wash); }
/* Submit row: button + Bombe submitting indicator on one baseline. */
.clarify-q-submit-row { display: flex; gap: 10px; align-items: center; }
.authoring-clarify { margin: 10px 0; }
.needs-you { margin: 14px 0 4px; }

/* Center stage */
.cockpit-stage { display: flex; flex-direction: column; min-width: 0; padding: 14px 18px; background: var(--page-tint); }
.stage-tabs { display: flex; gap: 6px; margin-bottom: 14px; }
.stage-tab {
  font-size: 10.5px; font-weight: 700; letter-spacing: .05em; color: var(--ink-faint);
  padding: 4px 10px; border-radius: 6px; background: var(--surface);
}
.stage-tab.on { background: var(--accent); color: #fff; }
.stage-panel { flex: 1; overflow-y: auto; }

.panel-h { font-size: 18px; font-weight: 700; color: var(--ink); margin: 0 0 4px; }
.panel-sub { font-size: 13.5px; color: var(--ink-soft); line-height: 1.5; margin: 0 0 16px; max-width: 60ch; }
.panel-sub.blocked { color: #b0432e; }

/* Executing panel */
.exec-agents { display: flex; flex-direction: column; gap: 10px; }
.exec-agent { border: 1px solid var(--line); border-left: 3px solid var(--ink-faint); background: var(--surface); border-radius: 9px; padding: 11px 13px; }
.exec-agent.gated { border-left-color: #2f8f5b; }
.exec-agent.exec { border-left-color: #2f5f9e; }
.exec-agent.pending { border-left-color: var(--ink-faint); }
.exec-agent-head { display: flex; align-items: center; gap: 10px; }
.exec-role { font-weight: 700; font-size: 13px; color: var(--ink); }
.exec-state { font-size: 11px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.exec-agent.gated .exec-state { color: #2f8f5b; }
.exec-agent.exec .exec-state { color: #2f5f9e; }
.exec-note { font-size: 12.5px; color: var(--ink-soft); margin: 5px 0 0; line-height: 1.4; }
/* Gate activity: the two layers named consistently and distinctly. */
.gate-activity { margin-top: 18px; }
.gate-activity-h { font-size: 11px; font-weight: 700; letter-spacing: .05em; color: var(--ink-faint); margin: 0 0 8px; }
.gate-event { display: flex; gap: 11px; align-items: flex-start; padding: 10px 0; border-top: 1px solid var(--line-soft); }
.gate-layer { flex: none; font-size: 10px; font-weight: 700; letter-spacing: .03em; padding: 3px 8px; border-radius: 5px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; white-space: nowrap; }
.gate-layer.l1 { background: rgba(176,67,46,0.18); color: #f87171; }   /* deny-before-execute */
.gate-layer.l2 { background: rgba(202,138,4,0.15); color: #fbbf24; }   /* post-task bounce */
.gate-event-text { font-size: 12.5px; color: var(--ink-soft); line-height: 1.45; margin: 0; }
.gate-rule { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; font-weight: 700; color: var(--ink); }

/* Done / provenance panel */
.prov-line { display: flex; gap: 12px; padding: 8px 0; border-bottom: 1px solid var(--line-soft); font-size: 13px; }
.prov-k { flex: none; width: 100px; color: var(--ink-faint); font-weight: 600; }
.prov-v { color: var(--ink); }

/* Blocked panel */
.blocked-reason { font-size: 13px; color: var(--ink-soft); line-height: 1.5; max-width: 60ch; background: rgba(176,67,46,0.12); border: 1px solid rgba(176,67,46,0.30); border-radius: 9px; padding: 12px 14px; }

/* Status strip (always visible under the stage) */
.status-strip { margin-top: 12px; border-top: 1px solid var(--line); padding-top: 12px; display: flex; align-items: center; gap: 16px; flex-wrap: wrap; }
.strip-fleet { display: flex; align-items: center; gap: 8px; }
.fleet-pill { display: inline-flex; align-items: center; gap: 7px; border: 1px solid var(--line); border-radius: 20px; padding: 4px 11px; background: var(--surface); }
.fleet-pill.gated { border-color: rgba(22,163,74,0.40); background: rgba(22,163,74,0.12); }
.fleet-pill.exec { border-color: #c2d4ec; background: #eef4fb; }
.fleet-role { font-size: 12.5px; font-weight: 600; color: var(--ink); }
.fleet-state { font-size: 10.5px; text-transform: uppercase; letter-spacing: .04em; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.fleet-pill.gated .fleet-state { color: #2f8f5b; }
.fleet-pill.exec .fleet-state { color: #2f5f9e; }
.fleet-arrow { color: var(--ink-faint); font-size: 13px; }
.strip-gates { display: flex; gap: 12px; margin-left: auto; }
.gate-tally { font-size: 11.5px; color: var(--ink-soft); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.gate-num { font-weight: 700; color: var(--accent-ink); }

/* Inspector */
.inspector-hint { font-size: 12px; color: var(--ink-faint); line-height: 1.4; margin: 0 0 12px; }
.rule-list { display: flex; flex-wrap: wrap; gap: 5px; margin-bottom: 14px; }
.rule-chip {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 10.5px;
  padding: 4px 8px; border-radius: 6px; cursor: pointer;
}
.rule-chip:hover { border-color: var(--ink-faint); }
.rule-chip.sel { border-color: var(--accent); background: var(--accent-wash); color: var(--accent-ink); }
.rule-detail { border-top: 1px solid var(--line); padding-top: 12px; }
.rule-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; font-weight: 700; color: var(--ink); margin: 0 0 6px; }
.rule-enforce { display: flex; align-items: center; gap: 7px; font-size: 12px; color: #2f8f5b; font-weight: 600; margin: 0 0 12px; }
.enforce-dot { width: 8px; height: 8px; border-radius: 50%; background: #2f8f5b; }
.rule-label { font-size: 10.5px; font-weight: 700; letter-spacing: .05em; color: var(--ink-faint); margin: 10px 0 4px; }
.rule-statement { font-size: 12.5px; color: var(--ink); line-height: 1.5; margin: 0; }

/* Cockpit loading / error / empty notice (shown while the BFF fetch resolves). */
.cockpit-notice {
  flex: 1; display: flex; flex-direction: column;
  align-items: center; justify-content: center; gap: 8px; text-align: center;
  padding: 40px; background: transparent;
}

/* Cockpit internal nav: control surface vs routines (both architect tools). */
.cockpit-nav { display: flex; gap: 4px; padding: 7px 16px; background: var(--paper); border-bottom: 1px solid var(--line); }
.cockpit-nav-tab {
  border: none; background: transparent; color: var(--ink-soft);
  font-size: 12.5px; font-weight: 700; padding: 5px 13px; border-radius: 7px; cursor: pointer;
}
.cockpit-nav-tab:hover { color: var(--ink); }
.cockpit-nav-tab.on { background: var(--surface); color: var(--ink); box-shadow: var(--shadow-card); }

/* Cumulative LLM usage meter — pinned to the right of the cockpit nav. Observability only. */
.usage-meter-wrap { margin-left: auto; position: relative; }
.usage-meter {
  display: inline-flex; align-items: center; gap: 5px;
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft);
  font-size: 11.5px; font-weight: 700; padding: 4px 10px; border-radius: 999px; cursor: pointer;
  font-variant-numeric: tabular-nums;
}
.usage-meter-loading { margin-left: auto; opacity: .7; }
.usage-meter:hover { color: var(--ink); border-color: var(--ink-faint); }
.usage-num { color: var(--ink); }
.usage-unit { color: var(--ink-faint); font-weight: 600; font-size: 10.5px; }
.usage-sep { color: var(--ink-faint); }
.usage-dim { color: var(--ink-faint); font-weight: 600; font-size: 11.5px; }
/* Amber rate-limited badge: distinct from the normal readout so it can't be missed. */
.usage-meter-rl {
  margin-left: auto;
  display: inline-flex; align-items: center; gap: 6px;
  border: 1px solid rgba(202,138,4,0.50); background: rgba(202,138,4,0.14); color: #fbbf24;
  font-size: 11.5px; font-weight: 800; padding: 4px 11px; border-radius: 999px;
}
.usage-rl-dot {
  width: 7px; height: 7px; border-radius: 50%; background: #d97a06;
  animation: usage-rl-pulse 1.1s ease-in-out infinite;
}
@keyframes usage-rl-pulse { 0%,100% { opacity: 1; } 50% { opacity: .35; } }
/* By-model breakdown dropdown. */
.usage-breakdown {
  position: absolute; right: 0; top: calc(100% + 6px); z-index: 50;
  background: var(--surface); border: 1px solid var(--line); border-radius: 9px;
  box-shadow: var(--shadow-card); padding: 8px; min-width: 280px;
}
.usage-breakdown-empty { font-size: 12px; color: var(--ink-faint); padding: 4px 6px; }
.usage-breakdown-table { width: 100%; border-collapse: collapse; font-size: 11.5px; font-variant-numeric: tabular-nums; }
.usage-breakdown-table th { text-align: left; color: var(--ink-faint); font-weight: 700; padding: 3px 6px; border-bottom: 1px solid var(--line); }
.usage-breakdown-table td { color: var(--ink-soft); padding: 3px 6px; border-bottom: 1px solid var(--line-soft); }
.usage-breakdown-table .usage-r { text-align: right; }

/* overflow-x MUST be visible (not hidden/auto/clip) so that position:fixed children
   (modals, overlays) resolve against the viewport rather than this scroll container.
   In WebKit-based webviews (wry), setting overflow-x:hidden on a scrolling ancestor
   makes position:fixed resolve against that ancestor — the root cause of "modal opens
   mid-page instead of viewport-centered".  Use clip-path or scrollbar-gutter to
   suppress unwanted horizontal scroll instead of overflow-x:hidden. */
/* The single page-filling layer present on EVERY cockpit page. It carries the one
   --page-tint over the Bombe. Anything nested inside it (e.g. .govdev-main) must stay
   transparent so the tint never stacks. */
.cockpit-scroll { flex: 1; overflow-y: auto; overflow-x: clip; min-width: 0; min-height: 0; background: var(--page-tint); }
.cockpit-notice-title { font-size: 18px; font-weight: 700; color: var(--ink); margin: 0; }
.cockpit-notice-body { font-size: 13.5px; color: var(--ink-soft); margin: 0; max-width: 44ch; line-height: 1.5; }

/* Run control + live governed run (Phase 3 execution). */
.btn-run {
  border: none; background: var(--accent); color: #fff;
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 8px;
  cursor: pointer; margin-bottom: 14px;
  transition: background .15s var(--ease);
}
.btn-run:hover { background: var(--accent-ink); }
.btn-run:disabled { background: var(--line); color: var(--ink-faint); cursor: not-allowed; }
/* In a button row, drop btn-run's standalone bottom margin so it aligns with the
   secondary buttons beside it (the margin is for standalone primary buttons). */
.findings-toolbar { align-items: center; }
.findings-toolbar .btn-run { margin-bottom: 0; }
/* The "Open" primary in a project card sits in a center-aligned action row beside the
   Export/Delete secondaries. Drop the standalone bottom margin and match their padding
   so all three buttons share the same baseline/height. */
.pg-card-actions .btn-run { margin-bottom: 0; padding: 8px 16px; }
/* Secondary button that MATCHES .btn-run's geometry (same padding/font/radius) so a
   primary + secondary pair in a modal/action row are balanced, not mismatched sizes.
   `.danger` tints the hover red for destructive actions (e.g. Start over). */
.btn-secondary {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink);
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 8px;
  cursor: pointer; transition: border-color .15s var(--ease), color .15s var(--ease);
}
.btn-secondary:hover:not(:disabled) { border-color: var(--accent); color: var(--accent-ink); }
.btn-secondary.danger:hover { border-color: #c0392b; color: #c0392b; }
.btn-secondary:disabled { opacity: .45; cursor: not-allowed; }
/* Stop control for an in-flight run/send. Matches .btn-secondary geometry but reads as a
   cancel affordance: a muted red outline that warms on hover. Used by the live-run Stop,
   the audit Stop, and the story-authoring Send's Stop. */
.btn-stop {
  border: 1px solid #d9b3ad; background: var(--surface); color: #c0392b;
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 8px;
  cursor: pointer; transition: border-color .15s var(--ease), background .15s var(--ease);
}
.btn-stop:hover:not(:disabled) { border-color: #c0392b; background: rgba(220,38,38,0.14); }
.btn-stop:disabled { opacity: .45; cursor: not-allowed; }
/* "+N" chip on a findings "type" cell: N other rule ids the server merged into this row
   (also_matches). Muted so it reads as a secondary annotation, not a second rule. */
.finding-also-count { color: var(--accent); font-weight: 700; font-size: 11px; cursor: help; }
/* Action row for the "leave onboarding?" confirm dialog: Cancel + Leave anyway. */
.onboard-leave-actions { display: flex; align-items: center; gap: 8px; justify-content: flex-end; margin-top: 16px; }
/* Drop btn-run's standalone bottom margin so the primary lines up with the secondary. */
.onboard-leave-actions .btn-run { margin-bottom: 0; }
/* Pre-audit cost estimate card: the price of the configured scan, before running it. */
.audit-cost { margin: 12px 0 4px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
.audit-cost-main { display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; }
.audit-cost-label { font-size: 12px; font-weight: 600; color: var(--ink-soft); text-transform: uppercase; letter-spacing: .04em; }
.audit-cost-val { font-size: 22px; font-weight: 800; color: var(--ink); }
.audit-cost-meta { font-size: 12px; color: var(--ink-faint); }
.audit-cost-note { margin: 6px 0 0; font-size: 12px; line-height: 1.5; color: var(--ink-faint); }
/* Deep-tier callout inside the cost note: amber + bold so the priciest option reads as a
   deliberate, expensive choice that's already baked into the figure above it. */
.audit-cost-deep-note { color: #854d0e; font-weight: 600; }
.live-run { border: 1px solid var(--line); border-radius: 11px; background: var(--surface); padding: 14px 16px; }
.live-run-head { display: flex; align-items: center; gap: 12px; margin-bottom: 4px; }
.live-run-title { font-size: 15px; font-weight: 700; color: var(--ink); }
.live-run-status { font-size: 11px; font-weight: 700; letter-spacing: .04em; padding: 3px 9px; border-radius: 6px; }
.live-events { display: flex; flex-direction: column; gap: 9px; margin-top: 12px; }
.live-event { border-left: 3px solid var(--ink-faint); border-radius: 0 8px 8px 0; background: var(--surface); padding: 9px 12px; }
.live-event.deny { border-left-color: #b0432e; background: rgba(176,67,46,0.14); }
.live-event.allow { border-left-color: #2f8f5b; background: rgba(22,163,74,0.10); }
.live-event.info { border-left-color: var(--ink-faint); background: var(--surface); }
.live-event.info .live-event-verdict { color: var(--ink-soft); }
.live-event.revise { border-left-color: #c08a2e; background: rgba(192,138,46,0.14); }
.live-event.revise .live-event-verdict { color: #b07d22; }
.live-event.delegate { border-left-color: #5b6fb0; background: rgba(91,111,176,0.12); }
.live-event.delegate .live-event-verdict { color: #4a5da0; }
.live-event.tier { border-left-color: #7a5bb0; background: rgba(122,91,176,0.12); }
.live-event.tier .live-event-verdict { color: #6a4aa0; }
.live-events-caption { font-size: 11.5px; color: var(--ink-faint); margin: 6px 0 0; }
.live-run-mode { font-size: 10.5px; font-weight: 700; letter-spacing: .03em; padding: 3px 8px; border-radius: 6px; background: var(--accent-wash); color: var(--accent-ink); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.live-event-head { display: flex; align-items: center; gap: 9px; margin-bottom: 3px; }
.live-event-verdict { font-size: 10.5px; font-weight: 700; letter-spacing: .05em; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.live-event.deny .live-event-verdict { color: #b0432e; }
.live-event.allow .live-event-verdict { color: #2f8f5b; }
.live-event-rule { font-size: 11px; font-weight: 700; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--ink); }
.live-event-detail { font-size: 12.5px; color: var(--ink-soft); line-height: 1.45; margin: 0; }
.live-events-empty { font-size: 12.5px; color: var(--ink-faint); font-style: italic; margin: 0; }

/* Clarify-bridge composer + thread (Phase 4). */
.clarify { margin-top: 22px; border-top: 1px solid var(--line); padding-top: 16px; }
.clarify-h { font-size: 15px; font-weight: 700; color: var(--ink); margin: 0 0 4px; }
.clarify-q {
  width: 100%; box-sizing: border-box; margin-top: 8px;
  border: 1px solid var(--line); border-radius: 9px; padding: 10px 12px;
  font: inherit; font-size: 13.5px; color: var(--ink); resize: vertical; background: var(--surface);
}
.clarify-q:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-wash); }
.clarify-label { font-size: 11px; font-weight: 700; letter-spacing: .05em; color: var(--ink-faint); margin: 12px 0 6px; }
.clarify-addressees { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; margin-bottom: 12px; }
.addressee-chip {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft);
  font-size: 12.5px; font-weight: 600; padding: 5px 11px; border-radius: 20px; cursor: pointer;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
.addressee-chip:hover { border-color: var(--ink-faint); }
.addressee-chip.sel { border-color: var(--accent); background: var(--accent-wash); color: var(--accent-ink); }
.addressee-input {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink);
  font: inherit; font-size: 12.5px; padding: 5px 10px; border-radius: 8px; min-width: 160px;
}
.addressee-input:focus { outline: none; border-color: var(--accent); }
.clarify-thread { display: flex; flex-direction: column; gap: 9px; margin-top: 14px; }
.clar-card { border: 1px solid var(--line); border-radius: 9px; padding: 10px 12px; background: var(--surface); }
.clar-card.open { border-left: 3px solid #d9a441; }
.clar-card.answered { border-left: 3px solid #2f8f5b; }
.clar-card-q { font-size: 13px; font-weight: 600; color: var(--ink); margin: 0 0 2px; line-height: 1.35; }
.clar-card-meta { font-size: 11.5px; color: var(--ink-faint); margin: 0 0 8px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.clar-answer-row { display: flex; gap: 8px; align-items: center; }
.clar-answer-row .addressee-input { flex: 1; }
.clar-answered { background: rgba(22,163,74,0.10); border-radius: 8px; padding: 8px 10px; }
.clar-answer-by { font-size: 11px; font-weight: 700; color: #2f8f5b; }
.clar-answer-text { font-size: 13px; color: var(--ink); margin: 3px 0 0; line-height: 1.4; }

/* Decomposition (Phase: story decomposition). */
.decompose { margin-top: 22px; border-top: 1px solid var(--line); padding-top: 16px; }
.proposed-list { display: flex; flex-direction: column; gap: 8px; margin-top: 12px; }
.proposed-child { display: flex; align-items: center; gap: 9px; }
.proposed-kind {
  flex: none; min-width: 42px; text-align: center; font-size: 10.5px; font-weight: 700;
  letter-spacing: .04em; padding: 4px 8px; border-radius: 6px; background: var(--accent-wash);
  color: var(--accent-ink); font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
.proposed-title { flex: 1; }
.children-list { margin-top: 14px; }
.child-row { display: flex; align-items: baseline; gap: 10px; padding: 6px 0; border-bottom: 1px solid var(--line-soft); }
.child-id { flex: none; font-size: 11.5px; font-weight: 700; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--accent-ink); }
.child-title { font-size: 13px; color: var(--ink); }

/* Routine dashboard (third surface). */
.routines-page { max-width: 980px; }
/* Status-at-a-glance strip: count pills above the table that double as filters. */
.routine-status-strip { display: flex; flex-wrap: wrap; gap: 8px; margin: 18px 0 4px; }
.routine-stat-pill { display: inline-flex; align-items: center; gap: 6px; padding: 5px 12px; border: 1px solid var(--line); border-radius: 999px; background: var(--surface); color: var(--ink-soft); cursor: pointer; transition: border-color .15s var(--ease), background .15s var(--ease); }
.routine-stat-pill:hover { border-color: var(--accent); }
.routine-stat-pill.on { border-color: var(--accent); background: var(--accent-wash); color: var(--accent-ink); }
.routine-stat-n { font-weight: 800; font-size: 13px; }
.routine-stat-label { text-transform: uppercase; letter-spacing: .03em; font-size: 10.5px; }
.routine-stat-pill.blocked .routine-stat-n { color: #c0392b; }
.routine-stat-pill.blocked.on { border-color: #e3b7ac; background: rgba(192,57,43,0.1); color: #c0392b; }
.routine-stat-pill.due .routine-stat-n { color: var(--accent-ink); }
/* Next-fire subline in the schedule cell. */
.routine-next-fire { display: block; font-size: 11px; color: var(--ink-faint); margin-top: 2px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.routine-next-fire.due-soon { color: var(--accent-ink); font-weight: 600; }
.routine-table { margin-top: 14px; border: 1px solid var(--line); border-radius: 11px; overflow: hidden; background: var(--surface); }
.routine-row {
  display: grid; grid-template-columns: 2.4fr 1fr 1.4fr 1.4fr auto; gap: 14px;
  align-items: center; padding: 12px 16px; border-bottom: 1px solid var(--line-soft);
}
.routine-row:last-child { border-bottom: none; }
.routine-head { background: var(--paper); font-size: 11px; font-weight: 700; letter-spacing: .05em; color: var(--ink-faint); }
.routine-row.off { opacity: .55; }
.routine-name { display: flex; flex-direction: column; gap: 3px; }
.routine-title { font-size: 13.5px; font-weight: 600; color: var(--ink); }
/* Title + lifecycle status badge on one line (issue #43). */
.routine-title-row { display: flex; align-items: center; gap: 8px; }
.routine-status-badge {
  padding: 1px 7px; border-radius: 999px;
  font-size: 10px; font-weight: 700; letter-spacing: .03em;
  text-transform: uppercase; border: 1px solid var(--line);
  color: var(--ink-soft); background: var(--surface-2, transparent);
}
.routine-status-badge.idle { color: var(--ink-faint); }
.routine-status-badge.running {
  color: var(--accent-ink);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
  border-color: color-mix(in srgb, var(--accent) 30%, transparent);
}
.routine-status-badge.done {
  color: #2f8f5b;
  background: color-mix(in srgb, #2f8f5b 12%, transparent);
  border-color: color-mix(in srgb, #2f8f5b 30%, transparent);
}
.routine-status-badge.failed, .routine-status-badge.blocked {
  color: #b4452f;
  background: color-mix(in srgb, #b4452f 12%, transparent);
  border-color: color-mix(in srgb, #b4452f 30%, transparent);
}
.routine-prompt { font-size: 12px; color: var(--ink-soft); line-height: 1.35; }
.routine-sched { font-size: 12.5px; color: var(--ink); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.routine-scope { font-size: 12px; color: var(--ink-soft); }
.routine-last { font-size: 12px; }
.routine-passed { color: #2f8f5b; font-weight: 600; }
.routine-never { color: var(--ink-faint); font-style: italic; }
/* An imported routine that hasn't been set up on this backend yet. */
.routine-needs-setup {
  margin-left: 8px; padding: 1px 7px; border-radius: 999px;
  font-size: 10.5px; font-weight: 700; letter-spacing: .02em;
  text-transform: uppercase;
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent-ink); border: 1px solid color-mix(in srgb, var(--accent) 30%, transparent);
}
.btn-setup { border-color: var(--accent); color: var(--accent-ink); font-weight: 700; }
/* Project group header in the routine table. Routines run globally; grouping is org only. */
.routine-group-head {
  grid-column: 1 / -1;
  padding: 14px 4px 6px; border-bottom: 1px solid var(--line); margin-bottom: 2px;
}
.routine-group-name {
  font-size: 11px; font-weight: 800; letter-spacing: .06em; text-transform: uppercase;
  color: var(--ink-faint);
}
.routine-actions { display: flex; gap: 6px; justify-content: flex-end; }
.btn-run-sm {
  border: none; background: var(--accent); color: #fff; font-size: 12px; font-weight: 600;
  padding: 5px 11px; border-radius: 8px; cursor: pointer;
}
.btn-run-sm:hover { background: var(--accent-ink); }
.routine-create { margin-top: 20px; border-top: 1px solid var(--line); padding-top: 16px; }
.routine-create-row { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 8px; }
.routine-create-row .addressee-input { flex: 1; min-width: 140px; }
.routine-intent-input { width: 100%; box-sizing: border-box; margin: 8px 0; padding: 9px 11px; border: 1px solid var(--line); border-radius: 8px; font: inherit; font-size: 13px; resize: vertical; }
.routine-draft-row { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
.routine-authored { font-size: 12px; color: var(--ink-soft); font-style: italic; }
.routine-prompt-input { width: 100%; box-sizing: border-box; margin-bottom: 10px; padding: 9px 11px; border: 1px solid var(--line); border-radius: 8px; font: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.5; resize: vertical; }

/* Empty / edit states + row action buttons. */
.routine-empty { padding: 16px; font-size: 13px; color: var(--ink-soft); }
.routine-row.editing { background: var(--accent-wash); }
.btn-edit-sm {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink); font-size: 12px; font-weight: 600;
  padding: 5px 11px; border-radius: 8px; cursor: pointer;
}
.btn-edit-sm:hover { border-color: var(--accent); color: var(--accent-ink); }
.btn-delete-sm {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft); font-size: 12px; font-weight: 600;
  padding: 5px 11px; border-radius: 7px; cursor: pointer;
}
.btn-delete-sm:hover { border-color: #c0392b; color: #c0392b; }
.btn-delete-sm.confirm { background: #c0392b; border-color: #c0392b; color: #fff; }

/* Structured schedule picker. */
.sched-picker { margin: 4px 0 12px; padding: 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
.sched-freq { display: flex; gap: 6px; flex-wrap: wrap; margin-bottom: 12px; }
.sched-freq-btn {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft); font-size: 12.5px; font-weight: 600;
  padding: 6px 14px; border-radius: 999px; cursor: pointer; transition: all .15s var(--ease);
}
.sched-freq-btn:hover { border-color: var(--accent); color: var(--accent-ink); }
.sched-freq-btn.on { background: var(--accent); border-color: var(--accent); color: #fff; }
.sched-detail { display: flex; flex-wrap: wrap; align-items: flex-end; gap: 16px; }
.sched-dow { display: flex; gap: 5px; flex-wrap: wrap; }
.sched-dow-btn {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft); font-size: 11.5px; font-weight: 600;
  width: 38px; padding: 6px 0; border-radius: 7px; cursor: pointer; text-align: center;
}
.sched-dow-btn:hover { border-color: var(--accent); }
.sched-dow-btn.on { background: var(--accent); border-color: var(--accent); color: #fff; }
.sched-field { display: flex; flex-direction: column; gap: 4px; }
.sched-field > span { font-size: 11px; font-weight: 700; letter-spacing: .04em; color: var(--ink-faint); text-transform: uppercase; }
.sched-field .addressee-input { flex: none; min-width: 0; }
.sched-num { width: 80px; }
.sched-scope-field { flex: 1; min-width: 200px; }
.sched-scope-hint { margin: -2px 0 12px; line-height: 1.5; }
.sched-preview { margin: 12px 0 0; font-size: 12.5px; color: var(--ink-soft); }
.sched-preview-val { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--ink); font-weight: 600; }
.routine-save-row { display: flex; gap: 10px; align-items: center; }

/* ---- research chat bubble (floating overlay) ------------------------------ */
.chat-fab {
  /* A rounded SQUARE, not a SaaS-template circle FAB — reads as a tool affordance. */
  position: fixed; right: 22px; bottom: 22px; z-index: 1000;
  width: 46px; height: 46px; border-radius: var(--r-md);
  border: 1px solid var(--accent-ink);
  background: var(--accent); color: #fff; font-size: 19px; cursor: pointer;
  box-shadow: var(--shadow-card); transition: transform .15s var(--ease), background .15s var(--ease);
}
.chat-fab:hover { background: var(--accent-ink); transform: translateY(-1px); }
.chat-panel {
  position: fixed; right: 22px; bottom: 86px; z-index: 1000;
  width: 380px; max-width: calc(100vw - 44px); height: 520px; max-height: calc(100vh - 130px);
  display: flex; flex-direction: column;
  background: var(--surface); border: 1px solid var(--line); border-radius: var(--r-md);
  box-shadow: var(--shadow-pop); overflow: hidden;
}
/* The header holds the title + the mode tabs + the model select + the backend badge.
   At 380px these don't fit on one row (the model select got clipped), so the header WRAPS:
   the title claims the first row, the controls flow onto the next. */
.chat-head { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 10px 12px; border-bottom: 1px solid var(--line); }
.chat-title { font-size: 13px; font-weight: 700; color: var(--ink); flex: 1 1 100%; }
.chat-model { font-size: 12px; padding: 4px 6px; border: 1px solid var(--line); border-radius: 6px; background: var(--paper); flex: 1 1 auto; min-width: 0; }
.chat-backend { font-size: 10px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase; color: var(--ink-faint); background: var(--paper); border: 1px solid var(--line); border-radius: 5px; padding: 2px 6px; }
.chat-disclaimer { font-size: 10.5px; color: var(--ink-faint); margin: 0; padding: 6px 12px; border-bottom: 1px solid var(--line-soft); }
.chat-log { flex: 1; overflow-y: auto; padding: 12px; display: flex; flex-direction: column; gap: 10px; }
.chat-empty { font-size: 12.5px; color: var(--ink-faint); margin: auto; text-align: center; }
.chat-turn { display: flex; flex-direction: column; gap: 3px; max-width: 92%; }
.chat-turn.you { align-self: flex-end; align-items: flex-end; }
.chat-turn.ai { align-self: flex-start; }
.chat-turn-role { font-size: 10px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase; color: var(--ink-faint); }
.chat-turn-text { font-size: 13px; line-height: 1.5; color: var(--ink); white-space: pre-wrap; word-break: break-word; padding: 8px 11px; border-radius: 12px; background: var(--paper); border: 1px solid var(--line-soft); }
.chat-turn.you .chat-turn-text { background: var(--accent-wash); border-color: var(--accent-wash); }
.chat-turn-text.dim { color: var(--ink-faint); font-style: italic; }
/* The floating ChatBubble's rendered-markdown AI reply (inline-styled bubble; this class targets
   the injected HTML so long lines + fenced code blocks WRAP instead of forcing a horizontal
   scroll out of the chat box). */
/* The floating research chat panel. Translucency + scroll live HERE (a global rule, the same path
   the terminal's .term-panel uses and which provably applies) because an inline backdrop-filter
   rendered opaque in the wry/WebKit webview. !important so nothing inline/legacy overrides them. */
.research-chat-panel { background: rgba(255,255,255,0.6) !important; }
/* Scroll on the message pane directly via max-height + overflow, NOT flex:1. A flex child inside a
   max-height (not fixed-height) flex column resolves against an INDEFINITE height in WebKit, so it
   grows to content instead of scrolling — which is exactly what was happening (panel kept getting
   taller, no scrollbar). A concrete max-height makes overflow-y engage reliably; the panel still
   grows to fit small chats and the pane scrolls once it passes the cap. */
.research-chat-log { max-height: 420px !important; overflow-y: auto !important; min-height: 0 !important; }

.chat-ai-md { overflow-wrap: anywhere; word-break: break-word; min-width: 0; }
.chat-ai-md pre {
  white-space: pre-wrap; overflow-wrap: anywhere; word-break: break-word;
  max-width: 100%; overflow-x: auto; margin: 6px 0; padding: 8px 10px;
  background: rgba(15,23,42,0.06); border-radius: 6px; font-size: 0.82rem;
}
.chat-ai-md code { white-space: pre-wrap; overflow-wrap: anywhere; word-break: break-word; }
.chat-ai-md p, .chat-ai-md li, .chat-ai-md h1, .chat-ai-md h2, .chat-ai-md h3 { overflow-wrap: anywhere; }

/* Rendered-markdown assistant replies: normal whitespace + styled block elements. */
.chat-turn-text.md { white-space: normal; }
.chat-turn-text.md > :first-child { margin-top: 0; }
.chat-turn-text.md > :last-child { margin-bottom: 0; }
.chat-turn-text.md p { margin: 6px 0; line-height: 1.5; }
.chat-turn-text.md ul, .chat-turn-text.md ol { margin: 6px 0; padding-left: 20px; }
.chat-turn-text.md li { margin: 2px 0; }
.chat-turn-text.md h1, .chat-turn-text.md h2, .chat-turn-text.md h3 {
  font-size: 13.5px; font-weight: 700; letter-spacing: -.01em; margin: 10px 0 4px;
}
.chat-turn-text.md code {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px;
  background: var(--accent-wash); padding: 1px 5px; border-radius: 4px;
}
.chat-turn-text.md pre {
  background: var(--paper); border: 1px solid var(--line); border-radius: 6px;
  padding: 8px 10px; overflow-x: auto; margin: 6px 0;
}
.chat-turn-text.md pre code { background: none; padding: 0; }
.chat-turn-text.md table { border-collapse: collapse; margin: 6px 0; font-size: 12px; width: 100%; }
.chat-turn-text.md th, .chat-turn-text.md td {
  border: 1px solid var(--line); padding: 4px 8px; text-align: left; vertical-align: top;
}
.chat-turn-text.md th { background: var(--accent-wash); font-weight: 700; }
.chat-turn-text.md a { color: var(--accent-ink); }
.chat-turn-text.md strong { font-weight: 700; }
.chat-compose { display: flex; gap: 8px; padding: 10px 12px; border-top: 1px solid var(--line); align-items: flex-end; }
.chat-input { flex: 1; resize: none; font: inherit; font-size: 13px; padding: 8px 10px; border: 1px solid var(--line); border-radius: 8px; }
.chat-send { border: none; background: var(--accent); color: #fff; font-size: 13px; font-weight: 600; padding: 8px 14px; border-radius: 8px; cursor: pointer; }
.chat-send:disabled { opacity: .5; cursor: default; }
.chat-mode-toggle { display: flex; border: 1px solid var(--line); border-radius: 7px; overflow: hidden; flex-shrink: 0; }
.chat-mode-btn { border: none; background: transparent; color: var(--ink-soft); font-size: 11.5px; font-weight: 700; padding: 4px 10px; cursor: pointer; transition: background .12s var(--ease), color .12s var(--ease); }
.chat-mode-btn:not(:last-child) { border-right: 1px solid var(--line); }
.chat-mode-btn.active { background: var(--accent); color: #fff; }

/* ---- Project-aware chat (#54) -------------------------------------------- */
/*
 * .chat-finding-banner: shows which specific finding the Project chat is focused on.
 * Compact strip between the mode disclaimer and the message log.
 *
 * .ask-finding-btn: the "Ask about this finding" affordance that appears in the
 * audit findings table, triggering the Project chat mode pre-seeded with the finding.
 */
.chat-finding-banner {
  display: flex; align-items: center; gap: 8px; padding: 5px 12px;
  background: var(--accent-wash); border-bottom: 1px solid var(--accent-wash);
  font-size: 11.5px; flex-wrap: wrap;
}
.chat-finding-label { color: var(--ink-faint); font-weight: 600; }
.chat-finding-rule { font-weight: 700; color: var(--accent-ink); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.chat-finding-loc { color: var(--ink-soft); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 10.5px; }
/* "Ask about this finding" button in findings tables */
.ask-finding-btn {
  border: 1px solid var(--line); background: var(--paper); color: var(--accent-ink);
  font-size: 11px; font-weight: 600; padding: 3px 9px; border-radius: 6px;
  cursor: pointer; white-space: nowrap;
  transition: background .12s var(--ease), border-color .12s var(--ease);
}
.ask-finding-btn:hover { background: var(--accent-wash); border-color: var(--accent-ink); }

/* ---- in-app terminal (issue #38) ------------------------------------------ */
/*
 * Layout mirror of .chat-fab / .chat-panel, offset to the LEFT of the chat FAB
 * so both are always reachable. The FAB is a rounded square (matching chat-fab)
 * but uses the ink palette — a "tool" affordance, not a bright CTA.
 *
 * RUNTIME-TODO: xterm.js is injected from jsdelivr CDN. Offline or CSP-strict
 * environments will need xterm.js vendored locally (served from the BFF or
 * bundled as a data-URL). The CSS variables below keep the terminal colors
 * consistent with the app palette.
 */
.term-fab {
  /* Rounded square, ink-toned — reads as a tool affordance next to the terracotta chat FAB. */
  position: fixed; right: 76px; bottom: 22px; z-index: 1000;
  width: 46px; height: 46px; border-radius: var(--r-md);
  border: 1px solid var(--line);
  background: var(--ink); color: var(--paper); font-size: 14px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-weight: 700;
  cursor: pointer; letter-spacing: -.02em;
  box-shadow: var(--shadow-card); transition: transform .15s var(--ease), background .15s var(--ease);
}
.term-fab:hover { background: #2e2d2a; transform: translateY(-1px); }

.term-panel {
  position: fixed; right: 76px; bottom: 86px; z-index: 1000;
  width: 640px; max-width: calc(100vw - 96px); height: 420px; max-height: calc(100vh - 130px);
  display: flex; flex-direction: column;
  /* Transparent fill (frosted via backdrop-blur only): the xterm canvas on top carries the single
     translucent layer (rgba theme + allowTransparency in terminal.rs). Tinting here too would
     compound with the canvas to near-opaque, which is the bug this avoids. */
  background: transparent; backdrop-filter: blur(12px); -webkit-backdrop-filter: blur(12px);
  border: 1px solid #2e2d2a; border-radius: var(--r-md);
  box-shadow: var(--shadow-pop); overflow: hidden;
}
/* Hidden state for a closed-but-mounted panel. visibility:hidden (NOT display:none)
   keeps the layout box sized so the xterm canvas stays alive + painted across reopen;
   pointer-events:none lets clicks pass through the invisible box. */
.term-panel.term-hidden { visibility: hidden; pointer-events: none; }

/* Tab bar */
.term-tabs {
  display: flex; align-items: center; gap: 2px;
  padding: 6px 8px 0; background: #111110;
  border-bottom: 1px solid #2e2d2a; flex-shrink: 0; overflow-x: auto;
}
.term-tab {
  display: flex; align-items: center; gap: 6px;
  padding: 5px 12px 5px 10px;
  border: 1px solid transparent; border-bottom: none;
  border-radius: var(--r-sm) var(--r-sm) 0 0;
  background: transparent; color: var(--ink-faint); font-size: 12px;
  cursor: pointer; transition: background .15s var(--ease), color .15s var(--ease);
  white-space: nowrap;
}
.term-tab:hover { background: #2e2d2a; color: var(--paper); }
.term-tab.active { background: #1b1a18; color: var(--paper); border-color: #2e2d2a; }
.term-tab-label { font-size: 12px; }
.term-tab-close {
  font-size: 13px; line-height: 1; color: var(--ink-faint);
  padding: 1px 3px; border-radius: 4px;
  transition: background .12s var(--ease), color .12s var(--ease);
}
.term-tab-close:hover { background: #3a3834; color: #fff; }
.term-tab-add {
  padding: 5px 11px; border: none; background: transparent;
  color: var(--ink-faint); font-size: 16px; cursor: pointer; border-radius: 6px;
  transition: background .12s var(--ease), color .12s var(--ease); line-height: 1;
}
.term-tab-add:hover { background: #2e2d2a; color: var(--paper); }

/* Session body: fills remaining space */
.term-body {
  flex: 1; overflow: hidden; position: relative;
}

/* Each session div fills the body. Only the active one is display:block. */
.term-session {
  width: 100%; height: 100%;
  /* xterm.js positions its canvas absolutely inside this container. */
  position: relative; overflow: hidden;
}

/* Placeholder when no tabs are open */
.term-empty {
  display: flex; align-items: center; justify-content: center;
  width: 100%; height: 100%;
  font-size: 13px; color: #6c6862;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

/* ---- agent-activity drawer ------------------------------------------------ */
.agent-activity { margin: 10px 0; }
.agent-activity-toggle {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink);
  font-size: 12.5px; font-weight: 600; padding: 6px 12px; border-radius: 8px; cursor: pointer;
}
.agent-activity-toggle:hover:not(:disabled) { border-color: var(--accent); color: var(--accent-ink); }
.agent-activity-toggle:disabled { opacity: .5; cursor: default; }
.agent-drawer { margin-top: 10px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); overflow: hidden; }
.agent-drawer-empty { font-size: 12.5px; color: var(--ink-soft); padding: 14px; margin: 0; }
.agent-tabs { display: flex; gap: 6px; padding: 10px; flex-wrap: wrap; border-bottom: 1px solid var(--line); }
.agent-tab {
  display: flex; flex-direction: column; gap: 2px; text-align: left;
  border: 1px solid var(--line); background: var(--surface); border-radius: 8px; padding: 6px 10px; cursor: pointer;
}
.agent-tab.on { border-color: var(--accent); background: var(--accent-wash); }
.agent-tab.blocked { border-color: #e3b7ac; }
.agent-tab-role { font-size: 12.5px; font-weight: 600; color: var(--ink); }
.agent-tab-status { font-size: 10px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; }
.agent-tab-status.running { color: var(--accent-ink); }
.agent-tab-status.done { color: var(--good); }
.agent-tab-status.blocked { color: #c0392b; }
.agent-detail { padding: 12px; display: flex; flex-direction: column; gap: 6px; }
.agent-detail-label { font-size: 10.5px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase; color: var(--ink-faint); margin: 6px 0 0; }
.agent-prompt, .agent-output {
  margin: 0; padding: 10px 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.5;
  white-space: pre-wrap; word-break: break-word; max-height: 240px; overflow-y: auto;
}
.agent-output { background: #1b1a18; color: #ece9e3; border-color: #1b1a18; }

/* ---- AI clarify suggestions ----------------------------------------------- */
.clarify-suggest-row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; margin: 6px 0 8px; }
.clarify-suggestions { display: flex; flex-direction: column; gap: 6px; margin-bottom: 10px; }
.clarify-suggestion {
  text-align: left; border: 1px solid var(--line); background: var(--accent-wash); color: var(--ink);
  font-size: 12.5px; line-height: 1.4; padding: 8px 11px; border-radius: 8px; cursor: pointer;
}
.clarify-suggestion:hover { border-color: var(--accent); }

/* ---- fix audited items (governed remediation) ----------------------------- */
.fix-panel { margin: 18px 0; padding: 14px 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
.fix-row { display: flex; align-items: center; gap: 14px; padding: 8px 0; border-bottom: 1px solid var(--line-soft); }
.fix-row:last-of-type { border-bottom: none; }
.fix-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; font-weight: 600; color: var(--ink); flex: 1; }
.fix-count { font-size: 12.5px; color: var(--ink-soft); }
.fix-msg { font-size: 12.5px; color: var(--accent-ink); margin: 10px 0 0; line-height: 1.5; }

/* ---- suppression registry (audit view) ------------------------------------ */
.sups-panel { margin: 18px 0; padding: 14px 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
.sups-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.sups-list { display: flex; flex-direction: column; gap: 6px; margin-top: 10px; }
.sup-row { display: flex; align-items: center; gap: 10px; padding: 7px 0; border-bottom: 1px solid var(--line-soft); flex-wrap: wrap; font-size: 12.5px; }
.sup-row.stale { opacity: .7; }
.sup-rule { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-weight: 600; color: var(--ink); }
.sup-source { font-size: 10px; font-weight: 700; text-transform: uppercase; letter-spacing: .04em; padding: 2px 6px; border-radius: 5px; background: var(--paper); border: 1px solid var(--line); color: var(--ink-soft); }
.sup-source.inline { color: var(--accent-ink); border-color: var(--accent-wash); }
.sup-loc { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; color: var(--ink-faint); }
.sup-reason { color: var(--ink-soft); flex: 1; min-width: 120px; }
.sup-ticket { font-size: 11px; font-weight: 600; color: var(--accent-ink); }
.sup-who { font-size: 11px; color: var(--ink-faint); }
.sup-stale-tag { font-size: 10px; font-weight: 700; text-transform: uppercase; color: #c0392b; border: 1px solid #e3b7ac; border-radius: 5px; padding: 2px 6px; }
.ignore-reason { flex: 1; min-width: 160px; }
.ignore-ticket { width: 130px; flex: none; }
/* Suppressions registry — read-only table with its OWN scroll (so it never stretches the page),
   a sticky header, and a table-row override for .sup-row (which is flex in the legacy list view). */
.sups-scroll { max-height: 320px; overflow-y: auto; margin-top: 10px; border: 1px solid var(--line); border-radius: 8px; }
.sups-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.sups-table thead th { position: sticky; top: 0; background: var(--surface); text-align: left; padding: 6px 9px; font-size: 10.5px; font-weight: 800; text-transform: uppercase; letter-spacing: .03em; color: var(--ink-faint); border-bottom: 1px solid var(--line); }
.sups-table tbody td { padding: 6px 9px; border-top: 1px solid var(--line-soft); vertical-align: top; color: var(--ink-soft); }
.sups-table tr.sup-row { display: table-row; }
.sups-table tr.sup-row.stale { opacity: .65; }
.sup-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; color: var(--ink-faint); }
.sup-active-tag { font-size: 10px; font-weight: 700; text-transform: uppercase; color: var(--ink-faint); border: 1px solid var(--line); border-radius: 5px; padding: 2px 6px; }

/* ---- projects home (the gate) --------------------------------------------- */
.project-gate { flex: 1; width: 100%; overflow-y: auto; display: flex; justify-content: center; background: var(--paper); }
.pg-inner { width: 100%; max-width: 720px; padding: 56px 28px 80px; }
.pg-empty { font-size: 14px; color: var(--ink-soft); margin: 18px 0; }
.pg-list { display: flex; flex-direction: column; gap: 10px; margin: 24px 0; }
.pg-card { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; border: 1px solid var(--line); border-radius: var(--r-md); background: var(--surface); box-shadow: var(--shadow-card); }
.pg-card-main { display: flex; flex-direction: column; gap: 4px; }
.pg-card-name { font-size: 16px; font-weight: 700; color: var(--ink); }
.pg-card-meta { font-size: 12.5px; color: var(--ink-soft); }
.pg-card-actions { display: flex; gap: 8px; align-items: center; }
/* One consistent button style/size across the card actions (Export / Delete / Open). */
.pg-card-actions button { font-size: 13px; font-weight: 600; padding: 8px 16px; border-radius: 8px; cursor: pointer; border: 1px solid transparent; transition: all .15s var(--ease); }
.pg-btn-secondary { border-color: var(--line); background: var(--surface); color: var(--ink); }
.pg-btn-secondary:hover { border-color: var(--accent); color: var(--accent-ink); }
.pg-btn-danger { border-color: var(--line); background: var(--surface); color: var(--ink-soft); }
.pg-btn-danger:hover { border-color: #c0392b; color: #c0392b; }
.pg-btn-danger.confirm { background: #c0392b; border-color: #c0392b; color: #fff; }
.onboard-browse { margin: 8px 0 0; }

/* ── Greenfield scaffold form ──────────────────────────────────────────────── */
.gf-form { display: flex; flex-direction: column; gap: 18px; margin-bottom: 20px; }
.gf-field { display: flex; flex-direction: column; gap: 5px; }
.gf-label { font-size: 12px; font-weight: 600; color: var(--ink); }
.gf-hint { font-size: 12px; color: var(--ink-soft); line-height: 1.5; margin: 0; }
.gf-input { width: 100%; box-sizing: border-box; padding: 8px 10px; border: 1px solid var(--line); border-radius: 8px; font-size: 13px; font-family: inherit; color: var(--ink); background: var(--paper); }
.gf-input:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-wash); }
.gf-dir-row { display: flex; align-items: center; gap: 8px; }
.gf-dir-path { flex: 1; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink); background: var(--surface); border: 1px solid var(--line); border-radius: 7px; padding: 6px 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.gf-dir-empty { color: var(--ink-faint); font-style: italic; }
.gf-rules-list { display: flex; flex-direction: column; gap: 4px; max-height: 320px; overflow-y: auto; padding: 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--paper); }
.gf-rules-group-h { font-size: 11px; font-weight: 700; color: var(--ink-soft); text-transform: uppercase; letter-spacing: 0.04em; margin: 8px 0 3px; }
.gf-rules-group-h:first-child { margin-top: 0; }
.gf-rule-row { display: flex; align-items: baseline; gap: 6px; padding: 4px 2px; cursor: pointer; font-size: 12.5px; border-radius: 5px; transition: background .1s var(--ease); }
.gf-rule-row:hover { background: var(--surface); }
.gf-rule-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--accent); font-size: 12px; white-space: nowrap; }
.gf-rule-title { color: var(--ink); }
.gf-rule-domain { color: var(--ink-faint); font-size: 11px; }
/* Greenfield result panel */
.gf-result { margin-top: 16px; padding: 14px 16px; border-radius: 10px; }
.gf-result-ok { background: #f0faf3; border: 1px solid #a3d9b1; }
.gf-result-err { background: #fff5f5; border: 1px solid #f5c0c0; }
.gf-result-h { font-weight: 700; font-size: 14px; color: var(--ink); margin: 0 0 5px; }
.gf-result-msg { font-size: 13px; color: var(--ink-soft); margin: 0 0 10px; line-height: 1.5; }
.gf-result-files { margin: 8px 0; }
.gf-result-files-h { font-size: 12px; font-weight: 600; color: var(--ink-soft); margin: 0 0 4px; }
.gf-result-files ul { margin: 0; padding-left: 16px; }
.gf-result-file { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink); }
.gf-result-sha { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink-soft); margin: 6px 0 0; }
.gf-result-path { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink-soft); margin: 4px 0 0; }
.gf-result-next { font-size: 12px; color: var(--ink-soft); margin: 10px 0 0; line-height: 1.5; }
.scan-domains-note { font-size: 12.5px; line-height: 1.55; color: var(--ink-soft); background: var(--paper); border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: 8px; padding: 10px 14px; margin: 0 0 12px; }
.scan-domains-note b { color: var(--ink); }
.audit-cta { margin: 20px 0; padding: 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--accent-wash); }

/* ---- rule detail modal (click a row) -------------------------------------- */
/* Proposed-rules table: per-domain "select all" as a column-filter-style
   multi-select. Trigger opens a FIXED-HEIGHT, scrollable checkbox list. */
.onboard-final-step {
  margin-top: 16px; padding: 14px; border: 1px solid var(--accent);
  border-radius: 11px; background: var(--accent-wash);
}
/* Onboarding status + lifecycle action bar (auto-saved indicator, start-over, finish). */
.onboard-actionbar {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
  margin: 12px 0 4px;
}
.onboard-actionbar-spacer { flex: 1 1 auto; }
/* Drop btn-run's standalone bottom margin so "Complete onboarding" lines up with "Start over". */
.onboard-actionbar .btn-run { margin-bottom: 0; }
.onboard-saved {
  font-size: 12px; font-weight: 600; color: #2f8f5b;
}
.onboard-step-eyebrow {
  display: inline-block; font-size: 10.5px; font-weight: 800; letter-spacing: .06em;
  text-transform: uppercase; color: var(--accent); margin-bottom: 8px;
}

.job-progress-scope { display: flex; align-items: center; gap: 7px; flex-wrap: wrap; margin-top: 8px; }
.job-progress-scope-h { font-size: 11.5px; font-weight: 700; color: var(--ink-soft); }
.job-progress-repo {
  font-size: 11px; font-weight: 600; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  color: var(--ink); background: var(--surface); border: 1px solid var(--line);
  border-radius: 999px; padding: 2px 9px;
}
.job-progress-scope-note { font-size: 11px; color: var(--ink-faint); flex-basis: 100%; }

.repo-health { margin: 4px 0 16px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
.repo-health-ok { font-size: 12.5px; color: #166534; margin: 0; font-weight: 600; }
.repo-health-warn { margin-bottom: 8px; }
.repo-health-warn-h { font-size: 13px; font-weight: 800; color: #92400e; }
.repo-health-row { display: flex; align-items: center; gap: 9px; padding: 5px 0; flex-wrap: wrap; }
.repo-health-icon { font-weight: 800; font-size: 12px; width: 16px; text-align: center; }
.repo-health-icon.ok { color: #166534; }
.repo-health-icon.bad { color: #b45309; }
.repo-health-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; font-weight: 600; color: var(--ink); }
.repo-health-path { font-size: 11.5px; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.repo-health-reason { font-size: 12px; color: var(--ink-soft); flex: 1; min-width: 200px; }

.pg-onboard-badge {
  display: inline-block; font-size: 10px; font-weight: 800; letter-spacing: .03em;
  text-transform: uppercase; border-radius: 999px; padding: 1px 8px; margin-left: 8px;
  color: var(--ink-soft); background: var(--surface); border: 1px solid var(--line);
}
.pg-onboard-badge.done { color: #166534; background: #dcfce7; border-color: #bbf7d0; }

.nr-flag {
  display: inline-block; font-size: 10px; font-weight: 800; letter-spacing: .03em;
  text-transform: uppercase; color: #92400e; background: #fde68a; border-radius: 999px;
  padding: 1px 8px; white-space: nowrap;
}
.nr-reason { font-size: 11.5px; color: var(--ink-soft); }
.nr-inline { color: #92400e; }

.arm-note { font-size: 11.5px; color: var(--ink-soft); line-height: 1.5; margin: 8px 0 0; max-width: 90ch; }
.arm-note code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; background: var(--accent-wash); padding: 1px 5px; border-radius: 5px; }

/* Apply overwrite-confirm modal: per-repo list of governance files that will be clobbered. */
.apply-overwrite-list { display: flex; flex-direction: column; gap: 10px; margin: 4px 0 8px; max-height: 320px; overflow-y: auto; }
.apply-overwrite-repo { border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; background: var(--surface-2, rgba(0,0,0,0.02)); }
.apply-overwrite-repo-name { display: block; font-weight: 600; font-size: 12.5px; margin-bottom: 6px; }
.apply-overwrite-files { list-style: disc; margin: 0; padding-left: 18px; display: flex; flex-direction: column; gap: 3px; }
.apply-overwrite-files li { font-size: 12px; color: var(--ink-soft); }
.apply-overwrite-files code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; background: var(--accent-wash); padding: 1px 5px; border-radius: 5px; }

.triage-switch { display: flex; gap: 6px; margin: 4px 0 12px; flex-wrap: wrap; }
.triage-tab {
  display: inline-flex; align-items: center; gap: 7px;
  border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft);
  font-size: 12.5px; font-weight: 600; padding: 7px 13px; border-radius: 8px; cursor: pointer;
  transition: border-color .15s var(--ease), color .15s var(--ease), background .15s var(--ease);
}
.triage-tab:hover { border-color: var(--accent); color: var(--ink); }
.triage-tab.active { background: var(--accent); border-color: var(--accent); color: #fff; }
.triage-tab-count {
  font-size: 11px; font-weight: 800; min-width: 18px; text-align: center;
  padding: 1px 6px; border-radius: 999px; background: var(--accent-wash); color: var(--ink);
}
.triage-tab.active .triage-tab-count { background: rgba(255,255,255,.25); color: #fff; }
.triage-process {
  display: flex; align-items: center; gap: 12px; flex-wrap: wrap;
  margin: 12px 0; padding: 11px 13px;
  background: var(--accent-wash); border: 1px solid var(--line); border-radius: 9px;
}
.td-bucket {
  display: inline-block; font-size: 11px; font-weight: 700; padding: 2px 9px; border-radius: 999px;
}
.td-bucket.later { background: var(--accent-wash); color: var(--ink-soft); border: 1px solid var(--line); }
.td-bucket.now { background: #fde68a; color: #92400e; }

.repo-select {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
  margin: 4px 0 12px; padding: 9px 12px;
  background: var(--accent-wash); border: 1px solid var(--line); border-radius: 9px;
}
.repo-select-label { font-size: 12px; font-weight: 700; color: var(--ink); }
.repo-select-input {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink);
  font-size: 12.5px; font-weight: 600; padding: 6px 10px; border-radius: 8px; cursor: pointer;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  transition: border-color .15s var(--ease);
}
.repo-select-input:hover { border-color: var(--accent); }
.repo-select-hint { font-size: 11.5px; color: var(--ink-soft); flex: 1; min-width: 220px; }


/* "It's working" spinner — paired with Auditing… / running status text. */
@keyframes camerata-spin { to { transform: rotate(360deg); } }
.spinner {
  display: inline-block; width: 13px; height: 13px; vertical-align: -2px;
  margin-right: 7px; border-radius: 50%;
  border: 2px solid var(--accent-wash); border-top-color: var(--accent);
  animation: camerata-spin .7s linear infinite;
}
.spinner-sm { width: 10px; height: 10px; margin-right: 5px; border-width: 1.5px; }

/* BombeSpinner — a 4x2 bank of rotor drums (homage to the Bletchley Park Bombe).
   Each drum's rotor mark rotates; row speed + column phase are set inline. */
.bombe { display: inline-flex; flex-direction: column; gap: 3px; vertical-align: middle; }
.bombe-row { display: flex; gap: 3px; }
.bombe-drum {
  position: relative; width: 13px; height: 13px; border-radius: 50%;
  border: 1.5px solid var(--ink-faint);
  background: radial-gradient(circle at 50% 40%, var(--surface), var(--accent-wash));
  box-shadow: inset 0 0 2px rgba(0,0,0,.15);
}
.bombe-mark {
  position: absolute; top: 1px; left: 50%; width: 2px; height: 4.5px; margin-left: -1px;
  border-radius: 1px; background: var(--accent);
  transform-origin: 50% 5.5px;            /* pivot on the drum center */
  animation-name: camerata-spin;
  /* CLOCK-LIKE, not a smooth spin: 12 discrete clicks per revolution (the mark jumps
     30deg each tick). All drums in a row share duration + zero delay, so they tick in
     lockstep. `steps()` is what makes the motion choppy. */
  animation-timing-function: steps(12, end);
  animation-iteration-count: infinite;
}
/* Bombe + label rows used where the AI is thinking. */
.audit-thinking, .agent-thinking { display: flex; align-items: center; gap: 11px; margin: 10px 0; }
.audit-thinking-label, .agent-thinking-label {
  font-size: 12.5px; font-weight: 600; color: var(--ink-soft);
  letter-spacing: .02em; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
/* Audit model picker (user owns speed/thoroughness). */
.audit-model-row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; margin: 10px 0; }
.audit-model-label { font-size: 12.5px; font-weight: 700; color: var(--ink); }
.audit-model-select {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink);
  font-size: 12.5px; font-weight: 600; padding: 5px 9px; border-radius: 8px; cursor: pointer;
}
.audit-model-select:hover:not(:disabled) { border-color: var(--accent); }
.audit-model-select:disabled { opacity: .55; cursor: default; }
.audit-model-hint { font-size: 11.5px; color: var(--ink-soft); }
/* Thorough-calibration checkbox (#51). */
.audit-thorough-toggle { display: inline-flex; align-items: center; gap: 7px; font-size: 13px; color: var(--ink); cursor: pointer; }
.audit-thorough-toggle input { width: 15px; height: 15px; cursor: pointer; accent-color: var(--accent); }
.audit-mode-rec { font-size: 11.5px; font-weight: 700; color: var(--accent-ink); white-space: nowrap; }

/* Async-job live progress bar (Mode 3). */
.job-progress { display: flex; align-items: center; gap: 11px; margin: 10px 0; }
.job-progress-track {
  flex: 1; height: 8px; max-width: 360px; border-radius: 999px;
  background: var(--accent-wash); overflow: hidden;
}
.job-progress-fill {
  height: 100%; background: var(--accent); border-radius: 999px;
  transition: width .4s var(--ease);
}
.job-progress-label {
  font-size: 12px; font-weight: 600; color: var(--ink-soft);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; white-space: nowrap;
}

/* Scan-type selector (Part C): the two "what to scan" checkboxes. */
.scan-type-selector { display: flex; gap: 18px; flex-wrap: wrap; align-items: center; }

/* Deterministic-scan progress — rendered ABOVE the AI agent-activity drawer. Styled to
   sit consistently with the async-job progress bar above. */
.det-progress {
  margin: 12px 0; padding: 12px 14px; border: 1px solid var(--line);
  border-radius: 10px; background: var(--surface);
}
.det-progress-head { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; }
.det-progress-title { font-size: 12.5px; font-weight: 800; color: var(--ink); }
.det-progress-count {
  font-size: 11.5px; font-weight: 600; color: var(--ink-soft);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; white-space: nowrap;
}
.det-progress-track {
  height: 8px; border-radius: 999px; background: var(--accent-wash);
  overflow: hidden; margin: 8px 0 10px;
}
.det-progress-fill {
  height: 100%; background: var(--accent); border-radius: 999px;
  transition: width .4s var(--ease);
}
.det-progress-tools { display: flex; flex-direction: column; gap: 5px; }
.det-tool { display: flex; align-items: center; gap: 8px; font-size: 12px; }
.det-tool-glyph { width: 14px; text-align: center; font-weight: 700; }
.det-tool-name { font-weight: 600; color: var(--ink); min-width: 110px; }
.det-tool-findings, .det-tool-state {
  font-size: 11px; color: var(--ink-soft);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
.det-tool-done .det-tool-glyph { color: #2f8f5b; }
.det-tool-running .det-tool-glyph { color: var(--accent); }
.det-tool-starting { opacity: .65; }
.det-progress-note { display: block; font-size: 11px; color: var(--ink-faint); margin-top: 9px; line-height: 1.5; }

/* Estimated token-usage badge in the Agent-activity detail. */
.agent-tokens {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 10.5px;
  font-weight: 700; color: var(--accent-ink); background: var(--accent-wash);
  border-radius: 999px; padding: 1px 8px; margin-left: 6px; letter-spacing: .02em;
}
.rule-modal-overlay { position: fixed; inset: 0; z-index: 1100; background: rgba(27,26,24,.34); display: flex; align-items: center; justify-content: center; padding: 24px; }
.rule-modal { width: 100%; max-width: 640px; max-height: 84vh; overflow-y: auto; background: #16130f; color: var(--ink); border-radius: var(--r-md); box-shadow: var(--shadow-pop); padding: 22px 24px; }
.rule-modal-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.rule-modal-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; font-weight: 700; color: var(--accent-ink); }
.rule-modal-close { border: none; background: transparent; font-size: 16px; color: var(--ink-soft); cursor: pointer; padding: 2px 6px; }
.rule-modal-close:hover { color: var(--ink); }
/* Work-item detail modal: a prominent top-right close button (the inline ✕ read as too
   subtle next to the title), and a focus-outline reset for chorale's inline-styled pager
   buttons so clicking a page doesn't leave the webview's native focus chrome on them. */
.wi-detail-modal { position: relative; }
.wi-detail-modal .rule-modal-close {
  position: absolute; top: 12px; right: 14px; z-index: 1;
  font-size: 20px; line-height: 1; padding: 6px 11px; border-radius: 8px;
  background: var(--paper); color: var(--ink-soft);
}
.wi-detail-modal .rule-modal-close:hover { background: var(--accent-wash); color: var(--ink); }
.wi-detail-modal .wi-detail-head { padding-right: 44px; }
/* Strip the system webview's native button chrome (beveled/gray / blue focus glow) that
   shows through on chorale's inline-styled pager buttons on initial render AND after
   page-change re-renders.  appearance + box-shadow are not set inline, so this stylesheet
   rule wins and lets chorale's own border/bg/shadow render consistently on every render.
   BUG D: in wry/WebKit, `:focus-visible` may match on mouse clicks (non-standard), so
   the `:focus:not(:focus-visible)` guard never fires.  We also set `:focus` directly so
   the native focus ring is suppressed regardless of how the webview evaluates :focus-visible. */
.chorale-root button {
  -webkit-appearance: none; appearance: none;
  box-shadow: none;   /* kill native focus glow that persists after page-change clicks */
  outline: none;      /* belt-and-suspenders: wry sometimes retains focus outline on re-render */
}
.chorale-root button:focus { outline: none; box-shadow: none; }
.chorale-root button:focus:not(:focus-visible) { outline: none; box-shadow: none; }
.rule-modal-title-row {
  display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap;
  margin: 8px 0 12px;
}
/* Drop the bottom-margin from the title itself — the row wrapper owns it now. */
.rule-modal-title-row .rule-modal-title { margin: 0; }
.rule-modal-title { font-size: 17px; font-weight: 700; color: var(--ink); margin: 8px 0 12px; line-height: 1.35; }

/* ── Verification / provenance badges ───────────────────────────────────────
 *
 * Four rungs of the rule grounding ladder, displayed:
 *   - next to the rule NAME in the rule detail modal (.rule-modal-title-row)
 *   - as a "Provenance" column in all three rule tables (proposed-rules in
 *     onboarding, corpus + applied in the Rules window).
 *
 * Design intent:
 *   verified      -> prominent GREEN checkmark — the one mark that means a human confirmed it.
 *   grounded      -> subtle BLUE pill — cited source, usable, but not human-signed-off yet.
 *   draft         -> muted GRAY italic — AI-generated, de-emphasized; not in the armed set.
 *   needs_recheck -> distinct AMBER warning — was verified, source drifted; needs attention.
 *
 * Hover tooltip carries the source citation for `grounded` / `verified`; the
 * browser's native `title` attribute provides it — no JS needed.
 */
.verif-badge {
  display: inline-block;
  font-size: 10px; font-weight: 700; letter-spacing: .04em;
  text-transform: uppercase; border-radius: 999px;
  padding: 2px 9px; white-space: nowrap;
  /* Tooltip cursor so the user knows there may be hover text. */
  cursor: default;
}

/* verified: GREEN + checkmark — prominent, the gold standard. */
.verif-badge-verified {
  background: #dcfce7; color: #166534; border: 1px solid #bbf7d0;
}

/* grounded: BLUE + circled source-dot (⦿) — cited source, machine-grounded, fully usable.
   The glyph (set in the label) makes grounded a CLEAR table status distinct from the verified
   checkmark and the symbol-less draft / needs-re-check badges, not just a faint blue tint.
   Solid border + saturated text keep it legible at the 10px table size. */
.verif-badge-grounded {
  background: #dbeafe; color: #1e40af; border: 1px solid #93c5fd;
}

/* draft: GRAY + italic — de-emphasized; these are NOT in the armed ruleset. */
.verif-badge-draft {
  background: var(--paper); color: var(--ink-faint);
  border: 1px solid var(--line); font-style: italic;
}

/* needs-recheck: AMBER — was verified, source drifted; does NOT wear the checkmark. */
.verif-badge-needs-recheck {
  background: #fef9c3; color: #854d0e; border: 1px solid #fde68a;
}
/* Full, wrapping explanation text in the finding-detail modal (the row cell truncates). */
.rule-modal-detail { font-size: 13.5px; color: var(--ink-soft); margin: 4px 0 12px; line-height: 1.55; white-space: pre-wrap; }
.rule-modal-meta { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 10px; }
.rule-modal-tag { font-size: 11px; font-weight: 600; color: var(--ink-soft); background: var(--paper); border: 1px solid var(--line); border-radius: 6px; padding: 3px 8px; }
.rule-modal-placement { font-size: 12.5px; color: var(--ink-soft); margin: 0 0 16px; line-height: 1.5; }
.rule-modal-note { font-size: 13px; color: var(--ink-soft); font-style: italic; }
.rule-modal-label { font-size: 11px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase; color: var(--ink-faint); margin: 0 0 8px; }
.rule-modal-opts { display: flex; flex-direction: column; gap: 8px; }
.rule-modal-section { margin: 0 0 16px; }
.rule-modal-question { font-size: 13.5px; color: var(--ink); line-height: 1.55; margin: 0; }
.rule-modal-why { font-size: 12.5px; color: var(--ink-soft); line-height: 1.55; margin: 0; }
.rule-modal-mustchoose { font-size: 12px; font-weight: 600; color: #92400e; background: #fde68a; border-radius: 7px; padding: 6px 10px; margin: 0 0 10px; }
.rule-opt { text-align: left; border: 1px solid var(--line); background: var(--paper); border-radius: 10px; padding: 12px 14px; cursor: pointer; display: flex; flex-direction: column; gap: 4px; transition: all .15s var(--ease); }
.rule-opt:hover { border-color: var(--accent); }
.rule-opt.on { border-color: var(--accent); background: var(--accent-wash); box-shadow: 0 0 0 1px var(--accent); }
.rule-opt-head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.rule-opt-label { font-size: 13.5px; font-weight: 600; color: var(--ink); }
.rule-opt-default-badge { font-size: 10px; font-weight: 800; letter-spacing: .04em; text-transform: uppercase; color: var(--ink-soft); background: var(--accent-wash); border: 1px solid var(--line); border-radius: 999px; padding: 1px 8px; }
.rule-opt-picked-badge { font-size: 10px; font-weight: 800; letter-spacing: .04em; text-transform: uppercase; color: #fff; background: var(--accent); border-radius: 999px; padding: 1px 8px; }
.rule-opt-directive { font-size: 12.5px; color: var(--ink-soft); line-height: 1.5; }
.rule-opt-why { font-size: 12px; color: var(--ink-faint); line-height: 1.5; font-style: italic; margin-top: 2px; }
.pg-create { margin-top: 32px; padding-top: 22px; border-top: 1px solid var(--line); }
.pg-create-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin: 10px 0; }
.pg-create-row .addressee-input { flex: 1; min-width: 180px; }
/* Drop btn-run's standalone bottom margin so it centers against the name input. */
.pg-create-row .btn-run { margin-bottom: 0; }
.pg-import { margin-top: 6px; }
.cockpit-nav-tab.back { color: var(--accent-ink); font-weight: 600; }
.cockpit-nav-tab.back:hover { background: var(--accent-wash); }
.cockpit-nav-tab.secondary { margin-left: auto; color: var(--ink-faint); font-weight: 600; }
.cockpit-nav-tab.secondary:hover { color: var(--ink-soft); }
.cockpit-nav-tab.secondary.on { color: var(--ink); }

/* ---- local workspace surface ---------------------------------------------- */
.ws-folder { margin: 18px 0 8px; padding: 14px 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
.ws-folder-row { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; }
.ws-path { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; color: var(--ink); flex: 1; min-width: 200px; word-break: break-all; }
.ws-path.none { color: var(--ink-faint); font-style: italic; font-family: inherit; }
.ws-hint { font-size: 13px; color: var(--ink-soft); line-height: 1.5; margin: 8px 0; }
.ws-project { margin-top: 18px; }
.ws-project-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; padding-bottom: 12px; border-bottom: 1px solid var(--line); margin-bottom: 12px; flex-wrap: wrap; }
.ws-repo { border: 1px solid var(--line); border-radius: 12px; padding: 14px 16px; margin-bottom: 12px; background: var(--surface); }
.ws-repo-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
.ws-repo-name { font-size: 14px; font-weight: 700; color: var(--ink); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.ws-repo-state { font-size: 12.5px; color: var(--ink-faint); }
.ws-repo-state.cloned { color: var(--good); font-weight: 600; }
.ws-repo-path { font-size: 11.5px; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; margin: 6px 0 10px; word-break: break-all; }
.ws-repo-actions { display: flex; align-items: flex-end; gap: 12px; flex-wrap: wrap; }
.ws-branch { width: 180px; }
.ws-title-field { flex: 1; min-width: 180px; }
.ws-repo-dirty { font-size: 12.5px; color: var(--accent-ink); margin: 10px 0 0; }
.ws-repo-pr { font-size: 12.5px; margin: 10px 0 0; }
.ws-repo-pr a { color: var(--accent-ink); }
.ws-repo-msg { font-size: 12.5px; color: var(--ink-soft); margin: 8px 0 0; }
.ws-project-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.ws-health { display: flex; gap: 8px; flex-wrap: wrap; margin: 10px 0 4px; }
.ws-health-stat { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; font-weight: 600; padding: 3px 10px; border-radius: 20px; border: 1px solid var(--line); background: var(--surface); color: var(--ink-soft); }
.ws-health-stat.ok   { background: rgba(22,163,74,0.10); border-color: rgba(22,163,74,0.28); color: #4ade80; }
.ws-health-stat.warn { background: rgba(202,138,4,0.14); border-color: rgba(202,138,4,0.30); color: #fbbf24; }
.ws-health-stat.bad  { background: rgba(220,38,38,0.14); border-color: rgba(220,38,38,0.28); color: #f87171; }
.ws-health-dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; flex-shrink: 0; }

/* ---- Git panel (issue #37) ------------------------------------------------ */
/* Compact panels that sit beneath the branch+ship row inside each .ws-repo.    */
.git-panel { margin-top: 18px; border-top: 1px solid var(--line-soft); padding-top: 14px; display: flex; flex-direction: column; gap: 14px; }
.git-section { display: flex; flex-direction: column; gap: 8px; }
.git-section-label { font-size: 12px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; color: var(--ink-faint); margin: 0; }
.git-log-hint { font-weight: 400; text-transform: none; letter-spacing: 0; font-size: 11px; color: var(--ink-faint); }
.git-branch-list { display: flex; flex-wrap: wrap; gap: 6px; }
.git-branch { display: inline-flex; align-items: center; gap: 6px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; padding: 4px 10px; border: 1px solid var(--line); border-radius: 999px; background: var(--surface); cursor: pointer; transition: border-color .2s var(--ease), background .2s var(--ease); }
.git-branch:hover { border-color: var(--accent); color: var(--accent-ink); }
.git-branch.current { background: var(--accent-wash); border-color: #efd9d0; cursor: default; font-weight: 700; }
.git-branch.current:hover { border-color: #efd9d0; color: var(--ink); }
.git-branch-name { color: inherit; }
.git-branch-current-mark { font-size: 10px; letter-spacing: .04em; font-weight: 700; color: var(--accent-ink); background: #fff; border: 1px solid #efd9d0; border-radius: 999px; padding: 1px 5px; }
.git-new-branch-row { display: flex; align-items: center; gap: 8px; }
.git-new-branch-input { flex: 1; font-size: 13px; padding: 6px 10px; }
.git-commit-row { display: flex; align-items: center; gap: 8px; }
.git-commit-input { flex: 1; font-size: 13px; padding: 6px 10px; }
.git-net-btns { display: flex; gap: 8px; align-items: center; }
.git-log { display: flex; flex-direction: column; gap: 4px; max-height: 280px; overflow-y: auto; }
.git-commit-row-log { display: grid; grid-template-columns: 1fr auto; grid-template-rows: auto auto; column-gap: 8px; row-gap: 2px; padding: 7px 10px; border: 1px solid var(--line-soft); border-radius: var(--r-sm); background: var(--surface); cursor: grab; transition: border-color .15s var(--ease); }
.git-commit-row-log:hover { border-color: var(--line); }
.git-commit-meta { grid-column: 1; grid-row: 1; display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
.git-commit-short { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; font-weight: 700; color: var(--accent-ink); }
.git-commit-date { font-size: 11px; color: var(--ink-faint); }
.git-commit-author { font-size: 11px; color: var(--ink-soft); }
.git-commit-subject { grid-column: 1; grid-row: 2; font-size: 12.5px; color: var(--ink); line-height: 1.4; }
.git-cherry-btn { grid-column: 2; grid-row: 1 / 3; align-self: center; font-size: 11px; padding: 4px 8px; white-space: nowrap; }
/* Status bar: one-line summary at the top of the git panel showing branch + sync + dirty state. */
.git-status-bar { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding: 7px 10px; border: 1px solid var(--line-soft); border-radius: var(--r-sm); background: var(--paper); }
.git-status-detail { font-size: 12.5px; color: var(--ink-soft); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; flex: 1; min-width: 0; }
/* Pill badges for sync state and dirty flag. */
.git-status-badge { font-size: 10.5px; font-weight: 700; letter-spacing: .04em; padding: 1px 7px; border-radius: 999px; white-space: nowrap; }
.git-status-dirty  { background: rgba(202,138,4,0.12); color: #fbbf24; border: 1px solid rgba(202,138,4,0.35); }
.git-status-sync   { background: #e8f5e9; color: #2e7d32; border: 1px solid #a5d6a7; }
.git-status-ahead  { background: var(--accent-wash); color: var(--accent-ink); border: 1px solid #efd9d0; }
.git-status-behind { background: #f3e8ff; color: #6a1b9a; border: 1px solid #ce93d8; }

/* ---- Rules view: project rules table (T1) + corpus table (T2) ------------- */

/* Toolbar below Table 1 (remove-from-repo action + hint). */
.rules-table-toolbar {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
  margin: 6px 0 16px; padding: 8px 10px;
  border: 1px solid var(--line); border-radius: 8px; background: var(--surface);
}
.rules-table-hint { font-size: 11.5px; color: var(--ink-soft); flex: 1; min-width: 200px; }

/* "Applied to" chips in Table 2 (corpus) — repo names rendered inline in the cell. */
.rule-repo-chip {
  display: inline-block; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px; padding: 1px 6px; border-radius: 999px;
  border: 1px solid var(--line); background: var(--accent-wash); color: var(--accent-ink);
  margin: 0 2px 2px 0;
}
.applied-to-empty { color: var(--ink-faint); font-size: 13px; }

/* Expandable "add-to-repo / go-to" panel below Table 2. */
.add-to-repo-details { margin: 8px 0 16px; }
.add-to-repo-summary {
  font-size: 12.5px; font-weight: 700; color: var(--ink-soft); cursor: pointer;
  padding: 7px 10px; border: 1px solid var(--line); border-radius: 8px;
  background: var(--surface); list-style: none; user-select: none;
}
.add-to-repo-summary:hover { color: var(--ink); border-color: var(--accent); }
.add-to-repo-list {
  margin-top: 4px; border: 1px solid var(--line); border-radius: 8px;
  background: var(--surface); overflow: hidden;
}
.add-to-repo-row {
  display: flex; align-items: center; gap: 10px; padding: 7px 12px; flex-wrap: wrap;
  border-bottom: 1px solid var(--line-soft); font-size: 12.5px;
}
.add-to-repo-row:last-child { border-bottom: none; }
.add-to-repo-rule-id {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-weight: 700;
  color: var(--accent-ink); white-space: nowrap; min-width: 140px;
}
.add-to-repo-rule-title { color: var(--ink-soft); flex: 1; min-width: 160px; }
.add-to-repo-select {
  border: 1px solid var(--line); background: var(--surface); color: var(--ink);
  font-size: 12px; padding: 4px 8px; border-radius: 7px; cursor: pointer;
  transition: border-color .15s var(--ease);
}
.add-to-repo-select:hover { border-color: var(--accent); }
.go-to-repo-btn {
  white-space: nowrap; font-size: 11.5px; padding: 3px 9px;
}

/* ── Docs view ─────────────────────────────────────────────────────────────── */
.docs-view {
  display: flex; flex-direction: column; height: 100%; padding: 24px 28px; gap: 16px;
}
.docs-tabs { display: flex; gap: 8px; flex-shrink: 0; }
.docs-body {
  flex: 1; overflow-y: auto; max-width: 860px;
  /* Inherit the chat markdown styling; add comfortable reading padding */
  padding: 20px 24px !important;
  font-size: 14px !important;
  line-height: 1.6 !important;
  border-radius: var(--r-md) !important;
}

/* ---- escalation (blocked routine) styles --------------------------------- */
/* Pill that appears on a routine row when the routine has an open escalation.
   Matches .routine-needs-setup's shape but reads as attention-state rather than
   a neutral warning: uses the full accent so it stands out in the table. */
.routine-blocked {
  display: inline-block;
  margin-left: 8px; padding: 2px 9px; border-radius: 999px;
  font-size: 10.5px; font-weight: 800; letter-spacing: .04em;
  text-transform: uppercase;
  background: var(--accent); color: #fff;
  border: 1px solid var(--accent-ink);
  cursor: pointer;
  transition: background .15s var(--ease);
}
.routine-blocked:hover { background: var(--accent-ink); }

/* The inline review panel that expands under a blocked routine row.
   Mirrors .routine-row.editing's accent-wash background so the pattern reads
   as a consistent "active / expanded" state in the table. */
.escalation-panel {
  background: var(--accent-wash);
  border-bottom: 1px solid color-mix(in srgb, var(--accent) 22%, transparent);
  padding: 16px 20px 20px;
  display: flex; flex-direction: column; gap: 12px;
  animation: slideIn .22s var(--ease) both;
}
.escalation-panel-head {
  display: flex; align-items: baseline; justify-content: space-between; gap: 12px;
}
.escalation-panel-name {
  font-size: 13.5px; font-weight: 700; color: var(--ink); letter-spacing: -.01em;
}
.escalation-panel-id {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 10.5px; color: var(--ink-faint);
}
.escalation-reason {
  font-size: 12.5px; color: var(--ink-soft); line-height: 1.5;
}
/* The key question the architect needs to answer: deserves visual emphasis. */
.escalation-stopped-for {
  font-size: 14px; font-weight: 600; color: var(--ink); line-height: 1.5;
  background: var(--surface); border: 1px solid var(--line);
  border-left: 3px solid var(--accent);
  border-radius: 0 var(--r-sm) var(--r-sm) 0;
  padding: 10px 14px;
}
.escalation-suggestions-label {
  font-size: 11px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase;
  color: var(--ink-faint); margin: 0 0 6px;
}
.escalation-suggestions { display: flex; flex-direction: column; gap: 6px; }
/* Each suggestion is a click-to-prefill affordance matching .clarify-suggestion. */
.escalation-suggestion {
  text-align: left; border: 1px solid var(--line); background: var(--surface); color: var(--ink);
  font-size: 12.5px; line-height: 1.4; padding: 8px 11px; border-radius: 8px; cursor: pointer;
  transition: border-color .15s var(--ease), background .15s var(--ease);
}
.escalation-suggestion:hover { border-color: var(--accent); background: var(--accent-wash); }
.escalation-answer-row {
  display: flex; flex-direction: column; gap: 8px;
}
.escalation-answer-input {
  width: 100%; box-sizing: border-box;
  border: 1px solid var(--line); border-radius: var(--r-sm); padding: 9px 11px;
  font: inherit; font-size: 13px; color: var(--ink); background: var(--surface);
  resize: vertical; line-height: 1.5;
  transition: border-color .2s var(--ease), box-shadow .2s var(--ease);
}
.escalation-answer-input:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-wash); }
.escalation-submit-row { display: flex; align-items: center; gap: 10px; }
/* Translated directive shown after a successful submit: calm positive feedback. */
.escalation-directive {
  margin-top: 6px; font-size: 12.5px; color: #4ade80; font-weight: 600;
  background: rgba(22,163,74,0.10); border: 1px solid rgba(22,163,74,0.30); border-radius: var(--r-sm);
  padding: 8px 12px; line-height: 1.5;
  animation: slideIn .3s var(--ease) both;
}

/* ---- escalation review conversation (lead-engineer chat) ----------------- */
.escalation-chat-thread {
  display: flex; flex-direction: column; gap: 10px;
  max-height: 320px; overflow-y: auto;
  padding: 10px; border: 1px solid var(--line); border-radius: var(--r-sm);
  background: var(--surface);
}
.escalation-turn { display: flex; flex-direction: column; gap: 3px; }
.escalation-turn-role {
  font-size: 10.5px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase;
  color: var(--ink-faint);
}
.escalation-turn.you .escalation-turn-role { color: var(--accent-ink); }
.escalation-turn-text { font-size: 13px; line-height: 1.55; color: var(--ink); }
/* Markdown from the lead engineer: compact table/code/list styling. */
.escalation-turn-text.md p { margin: 0 0 6px; }
.escalation-turn-text.md p:last-child { margin-bottom: 0; }
.escalation-turn-text.md code {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px;
  background: var(--accent-wash); padding: 1px 4px; border-radius: 4px;
}
.escalation-turn-text.md pre {
  background: var(--accent-wash); padding: 8px 10px; border-radius: var(--r-sm);
  overflow-x: auto; margin: 0 0 6px;
}
.escalation-turn-text.md ul, .escalation-turn-text.md ol { margin: 0 0 6px; padding-left: 18px; }
.escalation-chat-row { display: flex; flex-direction: column; gap: 8px; }
.escalation-chat-controls { display: flex; align-items: center; gap: 10px; }
.escalation-chat-label {
  font-size: 11px; font-weight: 700; letter-spacing: .05em; text-transform: uppercase;
  color: var(--ink-faint);
}
.escalation-model { max-width: 180px; font-size: 12px; padding: 4px 8px; }
/* The authorize section is visually separated: it's the only control that unblocks. */
.escalation-authorize {
  display: flex; flex-direction: column; gap: 8px;
  margin-top: 4px; padding-top: 12px; border-top: 1px solid var(--line);
}

/* Gate self-check (#14): in-app end-to-end gate-loop GO/NO-GO. */
.gate-selfcheck { margin: 10px 16px 0; padding: 10px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
.gate-selfcheck-head { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.gate-selfcheck-title { font-weight: 700; font-size: 13px; color: var(--ink); }
.gate-selfcheck-sub { font-size: 12px; color: var(--ink-soft); margin-right: auto; }
.gate-selfcheck-verdict { display: flex; align-items: flex-start; gap: 10px; margin-top: 9px; padding: 8px 11px; border-radius: 8px; }
.gate-selfcheck-verdict.go { background: rgba(60, 138, 76, 0.10); border: 1px solid rgba(60, 138, 76, 0.4); }
.gate-selfcheck-verdict.nogo { background: rgba(212, 51, 43, 0.08); border: 1px solid rgba(212, 51, 43, 0.45); }
.gate-selfcheck-badge { font-weight: 800; font-size: 13px; letter-spacing: .02em; }
.gate-selfcheck-verdict.go .gate-selfcheck-badge { color: #2f7d3f; }
.gate-selfcheck-verdict.nogo .gate-selfcheck-badge { color: #c0392b; }
.gate-selfcheck-lines { display: flex; flex-direction: column; gap: 2px; font-size: 12px; color: var(--ink-soft); }
.loop-guard { margin: 0 0 12px; padding: 10px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
.loop-guard-head { display: flex; flex-direction: column; gap: 2px; }
.loop-guard-title { font-weight: 700; font-size: 13px; color: var(--ink); }
.loop-guard-sub { font-size: 12px; color: var(--ink-soft); }
.loop-guard-row { display: flex; align-items: center; gap: 8px; margin-top: 9px; }
.loop-guard-input { width: 64px; text-align: center; padding: 4px 6px; border: 1px solid var(--line); border-radius: 6px; background: #11100f; color: var(--ink); font-size: 13px; }
.loop-guard-save { margin-left: 4px; }

/* ── Governed-development lifecycle strip (Pillar 2) ─────────────────────── */
.uow-lifecycle {
  display: flex; flex-direction: column; gap: 8px; margin-bottom: 12px;
  padding-bottom: 12px; border-bottom: 1px solid var(--line);
}
.uow-lifecycle-strip {
  display: flex; flex-wrap: wrap; gap: 6px; align-items: center;
}
/* Each stage pip: dim until reached, accented when reached, ringed when current. */
.uow-stage-pip {
  font-size: 10.5px; font-weight: 600; line-height: 1; white-space: nowrap;
  padding: 5px 9px; border-radius: 999px; border: 1px solid var(--line);
  background: var(--paper); color: var(--ink-faint);
}
.uow-stage-pip.reached {
  color: var(--ink); border-color: var(--accent); background: var(--surface);
}
.uow-stage-pip.current {
  color: #fff; background: var(--accent); border-color: var(--accent);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 28%, transparent);
}
.uow-lifecycle-actions { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
/* The transition action (e.g. Approve decisions) reuses the onboarding `.btn-secondary`
   variant directly — bordered, same geometry as the accent primary run button — so the
   UoW controls and the onboarding page read as one button system. */

/* ── Per-phase run control, inline with the lifecycle steps (Increment 1) ──── */
/* The run control for the ACTIVE phase renders here; it replaces the prior
   phase's control rather than stacking. */
.uow-step-control {
  display: flex; flex-direction: column; gap: 8px;
  padding: 12px; border: 1px solid var(--line); border-radius: 9px;
  background: var(--surface);
}
.uow-step-h {
  font-size: 11px; font-weight: 700; letter-spacing: .05em;
  text-transform: uppercase; color: var(--ink-faint); margin: 0;
}
/* The three per-tier model selects for a development run. */
.uow-tier-grid {
  display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px;
}
.uow-tier-field { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
.uow-tier-field .run-model-select { width: 100%; }
/* The run-button + model-select row inside a step control: give the side-by-side
   controls comfortable breathing room (TASK 2). */
.run-control-row { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
/* In a side-by-side row, drop btn-run's standalone bottom margin so the primary aligns
   with the model select beside it — same pattern the onboarding action rows use. The
   row's `gap` already provides the comfortable breathing room. */
.run-control-row .btn-run { margin-bottom: 0; }
/* The "Update branch" control's source-branch picker (grouped local/origin). */
.uow-branch-select {
  min-width: 200px; max-width: 360px; padding: 7px 9px;
  border: 1px solid var(--line); border-radius: 8px; font: inherit; font-size: 12px;
  background: var(--surface);
}
/* Per-repo "Update branch" header row: heading label + target-repo identifier. */
.uow-update-branch-repo-header {
  display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap;
}
.uow-update-branch-repo-label {
  font-size: 12px; color: var(--ink-soft);
}
/* Searchable combobox wrapper for the branch picker. */
.uow-branch-combobox { display: flex; flex-direction: column; min-width: 0; }
/* The text input that drives the native datalist filter. */
.uow-branch-input {
  min-width: 200px; max-width: 360px; padding: 7px 9px;
  border: 1px solid var(--line); border-radius: 8px; font: inherit; font-size: 12px;
  background: var(--surface);
}
/* One-time bootstrap escape-hatch toggle on the development run control: a clearly-
   labeled, default-off checkbox that skips ONLY layer-2 for the tool-installing run. */
.uow-bootstrap-toggle {
  display: flex; align-items: flex-start; gap: 8px; cursor: pointer;
  padding: 9px 11px; border: 1px solid var(--line); border-radius: 8px;
  background: var(--paper);
}
.uow-bootstrap-toggle input { margin-top: 2px; flex: none; cursor: pointer; }
.uow-bootstrap-text { display: flex; flex-direction: column; gap: 2px; }
.uow-bootstrap-label { font-size: 12.5px; font-weight: 600; color: var(--ink); }
.uow-bootstrap-hint { font-size: 11.5px; line-height: 1.45; color: var(--ink-faint); }

/* Frozen gate provenance read-out on the UoW panel. */
.uow-provenance {
  display: flex; flex-direction: column; gap: 3px; margin-bottom: 10px;
}
.uow-provenance-val { font-size: 12px; color: var(--ink); }
.uow-provenance-rules { font-size: 11px; color: var(--ink-soft); }
.uow-signoff-row { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; }

/* ── VCS-gate process-rule settings panel (issue #65) ────────────────────── */
.vcs-settings-panel {
  padding: 24px 28px;
  max-width: 760px;
  display: flex;
  flex-direction: column;
  gap: 24px;
  overflow-y: auto;
  overflow-x: hidden;   /* mirrors .credentials-panel fix: suppress implicit h-scrollbar */
}
.vcs-settings-loading {
  padding: 24px;
  color: var(--ink-soft);
  font-size: 14px;
}
.vcs-settings-title {
  margin: 0;
  font-size: 20px;
  font-weight: 600;
  color: var(--ink);
}
.vcs-settings-intro {
  margin: 0;
  font-size: 13px;
  color: var(--ink-soft);
  line-height: 1.5;
}
.vcs-settings-empty-hint {
  margin: 0;
  font-size: 13px;
  color: var(--ink-soft);
  line-height: 1.6;
  background: var(--surface);
  border: 1px dashed var(--line);
  border-radius: var(--r-md);
  padding: 16px 20px;
}
.vcs-settings-rule-section {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 16px 20px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.vcs-settings-rule-title {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
  color: var(--ink);
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}
.vcs-settings-rule-id {
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  font-size: 12px;
  background: var(--accent-wash);
  color: var(--accent-ink);
  border-radius: var(--r-sm);
  padding: 2px 6px;
}
.vcs-settings-opt-in-badge {
  font-size: 11px;
  font-weight: 500;
  background: var(--line-soft);
  color: var(--ink-soft);
  border-radius: var(--r-sm);
  padding: 2px 6px;
}
.vcs-settings-rule-desc {
  margin: 0;
  font-size: 13px;
  color: var(--ink-soft);
  line-height: 1.5;
}
.vcs-settings-toggle {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: var(--ink);
  cursor: pointer;
  user-select: none;
}
.vcs-settings-tunables {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding-top: 6px;
}
.vcs-settings-label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--ink-soft);
}
.vcs-settings-input {
  font-size: 13px;
  color: var(--ink);
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 5px 8px;
  width: 160px;
  outline: none;
  transition: border-color .15s var(--ease);
}
.vcs-settings-input:focus {
  border-color: var(--accent);
}
.vcs-settings-input-wide {
  width: 100%;
  max-width: 520px;
}
.vcs-settings-select {
  font-size: 13px;
  color: var(--ink);
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 5px 8px;
  width: 220px;
  outline: none;
  cursor: pointer;
}
.vcs-settings-hint {
  margin: 0;
  font-size: 11px;
  color: var(--ink-faint);
  line-height: 1.5;
}
.vcs-settings-story-id-fmt {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  background: var(--paper);
  border: 1px solid var(--line-soft);
  border-radius: var(--r-sm);
}
.vcs-settings-actions {
  display: flex;
  gap: 10px;
  align-items: center;
}
/* Bypass section */
.vcs-settings-bypass-section {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 16px 20px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.vcs-settings-textarea {
  font-size: 13px;
  color: var(--ink);
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 6px 8px;
  width: 100%;
  max-width: 520px;
  min-height: 72px;
  resize: vertical;
  outline: none;
  font-family: inherit;
  transition: border-color .15s var(--ease);
}
.vcs-settings-textarea:focus {
  border-color: var(--accent);
}
.vcs-settings-bypass-ok {
  margin: 4px 0 0;
  font-size: 13px;
  color: var(--good);
}
.vcs-settings-bypass-err {
  margin: 4px 0 0;
  font-size: 13px;
  color: #b3261e;
}
.vcs-settings-bypass-record {
  margin: 4px 0 0;
  font-size: 11px;
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 8px 10px;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 200px;
  overflow-y: auto;
}

/* ── Deep compliance & security tier (#55) ────────────────────────────── */
.deep-tier-warning {
  color: #b45309;
  font-weight: 500;
}
.deep-tier-panel {
  margin-top: 20px;
  padding: 16px 20px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.deep-tier-heading {
  font-size: 15px;
  font-weight: 600;
  margin: 0;
  color: var(--ink);
}
.deep-tier-disclaimer {
  font-size: 12px;
  color: #b45309;
  background: #fffbeb;
  border: 1px solid #fde68a;
  border-radius: var(--r-sm);
  padding: 10px 12px;
  margin: 0;
  line-height: 1.55;
}
.deep-lens {
  padding: 14px 16px;
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.deep-lens-heading {
  font-size: 14px;
  font-weight: 600;
  margin: 0;
  color: var(--ink);
}
.deep-lens-desc {
  font-size: 12px;
  color: var(--ink-2);
  margin: 0;
}
.deep-lens-disclaimer {
  font-size: 12px;
  color: #b45309;
  margin: 0;
  font-style: italic;
}
.deep-lens-summary {
  font-size: 13px;
  color: var(--ink);
  margin: 0;
  white-space: pre-wrap;
}
.deep-lens-detail {
  font-size: 12px;
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 10px 12px;
  white-space: pre-wrap;
  overflow-x: auto;
  margin: 0;
}

/* SOC-2 gap table (inside deep-security lens) */
.soc2-gap-table {
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-size: 12px;
}
.soc2-gap-row {
  display: grid;
  grid-template-columns: 80px 160px 72px 1fr 1fr;
  gap: 8px;
  padding: 6px 8px;
  border-radius: var(--r-sm);
  align-items: start;
}
.soc2-gap-row.header {
  font-weight: 600;
  background: var(--surface);
  color: var(--ink-2);
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: .04em;
}
.soc2-gap-row.soc2-status-gap { background: #fef2f2; }
.soc2-gap-row.soc2-status-partial { background: #fffbeb; }
.soc2-gap-row.soc2-status-met { background: #f0fdf4; }
.soc2-badge-gap { color: #b91c1c; font-weight: 600; }
.soc2-badge-partial { color: #b45309; font-weight: 600; }
.soc2-badge-met { color: #15803d; font-weight: 600; }
.soc2-badge-unknown { color: var(--ink-2); font-style: italic; }
.soc2-col-ctrl { font-family: ui-monospace, "SFMono-Regular", Consolas, monospace; }
.soc2-col-gap { color: #b91c1c; }

/* ── Model tier-map editor (#63) ──────────────────────────────────────── */
.tier-map-editor {
  padding: 14px 16px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.tier-map-heading {
  font-size: 14px;
  font-weight: 600;
  margin: 0;
  color: var(--ink);
}
.tier-map-hint {
  font-size: 12px;
  color: var(--ink-2);
  margin: 0;
}
.tier-map-rows {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.tier-map-row {
  display: grid;
  grid-template-columns: 90px 200px 1fr;
  gap: 10px;
  align-items: center;
}
.tier-map-band-label {
  font-size: 13px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: var(--r-sm);
  text-align: center;
}
.tier-map-fast { background: rgba(22,163,74,0.18); color: #4ade80; }
.tier-map-balanced { background: rgba(37,99,235,0.18); color: #93c5fd; }
.tier-map-strongest { background: rgba(107,33,168,0.20); color: #c084fc; }
.tier-map-band-desc {
  font-size: 12px;
  color: var(--ink-2);
}
.tier-map-input {
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  font-size: 12px;
  max-width: 100%;
  width: 100%;
  box-sizing: border-box;
  min-width: 0;
}
/* Tier-chain chain editor: the list of model dropdowns + add/remove controls fits
   within its 1fr grid column by forcing both the chain container and each select
   to stay within the available width. */
.tier-map-chain-row { grid-template-columns: 90px 1fr 1fr; }
.tier-chain-list { display: flex; flex-direction: column; gap: 4px; min-width: 0; max-width: 100%; }
.tier-chain-entry { display: flex; align-items: center; gap: 6px; min-width: 0; }
.tier-chain-select { flex: 1; min-width: 0; max-width: 100%; box-sizing: border-box; }
.tier-chain-input { flex: 1; min-width: 0; max-width: 100%; box-sizing: border-box; }

/* Designer (vision) band color: warm amber to distinguish from logic tiers. */
.tier-map-designer { background: rgba(217,119,6,0.18); color: #fbbf24; }

/* ── Designer (vision) subsection within TierMapEditor ──────────────── */
.tier-map-vision-section {
  margin-top: 16px;
  padding-top: 14px;
  border-top: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 10px;
}
/* Toggle row for vision-enabled: label + checkbox + hint inline. */
.vision-toggle-row { display: flex; align-items: center; gap: 10px; }

/* ── Info icon (ⓘ) inline hint marker with CSS hover tooltip ────────── */
.info-icon {
  display: inline-block;
  position: relative;   /* anchors the absolutely-positioned .info-tip */
  font-size: 11px;
  color: var(--ink-2);
  cursor: help;
  margin-left: 4px;
  vertical-align: middle;
  opacity: 0.7;
  user-select: none;
}
.info-icon:hover { opacity: 1; color: var(--accent); }
/* Tooltip bubble — hidden by default, appears on hover */
.info-tip {
  display: none;
  position: absolute;
  bottom: calc(100% + 6px);
  left: 50%;
  transform: translateX(-50%);
  background: #1a1715;
  color: var(--ink);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 6px 10px;
  font-size: 11.5px;
  font-style: normal;
  line-height: 1.45;
  white-space: normal;
  max-width: 260px;
  min-width: 120px;
  box-shadow: 0 4px 16px rgba(0,0,0,0.55);
  z-index: 9999;
  pointer-events: none;
  text-align: left;
}
.info-icon:hover .info-tip { display: block; }

/* Step-model label + info-icon wrapper (keeps them inline).
   The label column is wide enough so it never overlaps the select.
   The .step-model-row overrides .tier-map-row's 3-column grid to give the
   label column more room and the select min-width: 0 so it shrinks gracefully. */
.step-model-label-wrap { display: flex; align-items: center; gap: 0; min-width: 0; }
.step-model-row {
  /* Override the default 90px label column — step labels can be longer. */
  grid-template-columns: 160px 1fr;
  gap: 12px;
  flex-wrap: wrap;
}
.step-model-row .tier-map-band-label {
  white-space: normal;
  word-break: break-word;
  text-align: left;
}
.step-model-row .tier-map-input {
  min-width: 0;
  width: 100%;
}

/* ── Model-profile selector (ModelProfileEditor) ──────────────────────── */
/* Each option is its own distinct visual block: label on one line, muted   */
/* description beneath. Clicking the whole label row selects the radio.    */
.profile-selector-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.profile-option {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: var(--paper);
  cursor: pointer;
  transition: border-color .15s var(--ease), background .15s var(--ease);
}
.profile-option:hover { border-color: var(--ink-faint); }
.profile-option-selected {
  border-color: var(--accent);
  background: var(--accent-wash);
}
/* Visually hide the native radio dot while keeping it in the accessibility tree
   and allowing label-click to trigger onchange (display:none disables events
   in some WebKit embeddings; position:absolute + clip is the safe alternative). */
.profile-option input[type="radio"] {
  position: absolute;
  width: 1px;
  height: 1px;
  margin: 0;
  padding: 0;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
  pointer-events: none;
}
.profile-option-label-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.profile-option-label {
  font-size: 13.5px;
  font-weight: 600;
  color: var(--ink);
  line-height: 1.2;
}
.profile-option-active-badge {
  font-size: 9.5px;
  font-weight: 800;
  letter-spacing: .06em;
  text-transform: uppercase;
  color: #fff;
  background: var(--accent);
  border-radius: 999px;
  padding: 1px 7px;
}
.profile-option-desc {
  font-size: 12px;
  color: var(--ink-soft);
  line-height: 1.45;
}
.profile-apply-row { margin-top: 4px; }

/* ── Rules-window SETTINGS section label ─────────────────────────────── */
.settings-label {
  margin-top: 20px;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: .07em;
  text-transform: uppercase;
  color: #b45309;
  border-left: 3px solid #b45309;
  padding-left: 8px;
}

/* ── pw/cockpit-ui product wave ─────────────────────────────────────── */
/*
 * Feature 2: App-update banner + rule-drift notice
 * Feature 3: Single-rule editor
 * Feature 4: Deep-report export
 * Feature 5: Feature-flag gated affordances
 */

/* App-update banner (Feature 2) */
.app-update-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 16px;
  background: #eff6ff;
  border-bottom: 1px solid #bfdbfe;
  font-size: 13px;
  color: #1d4ed8;
  flex-shrink: 0;
}
.app-update-icon {
  font-size: 16px;
  line-height: 1;
}
.app-update-text {
  flex: 1;
}
.app-update-notes {
  font-style: italic;
  color: #2563eb;
  margin-left: 6px;
}
.app-update-link {
  color: #1d4ed8;
  text-decoration: underline;
  white-space: nowrap;
}
.app-update-dismiss {
  background: none;
  border: none;
  cursor: pointer;
  font-size: 16px;
  color: #6b7280;
  padding: 0 4px;
  line-height: 1;
}
.app-update-dismiss:hover { color: var(--ink); }

/* Rule-drift notice (Feature 2) */
.drift-notice {
  border: 1px solid #fcd34d;
  border-radius: var(--r-md);
  background: #fffbeb;
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-bottom: 16px;
}
.drift-notice-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.drift-notice-icon {
  font-size: 16px;
  margin-right: 6px;
}
.drift-notice-title {
  font-size: 14px;
  font-weight: 600;
  color: #92400e;
}
.drift-notice-hint {
  font-size: 12px;
  color: #78350f;
  margin: 0;
}
.drift-entry {
  border: 1px solid #fde68a;
  border-radius: var(--r-sm);
  background: var(--surface);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.drift-entry-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.drift-entry-id {
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  font-size: 12px;
  font-weight: 700;
  color: var(--ink);
  background: var(--line-soft);
  padding: 2px 6px;
  border-radius: var(--r-sm);
}
.drift-entry-title {
  font-size: 13px;
  color: var(--ink-soft);
}
.drift-entry-repos {
  font-size: 11px;
  color: var(--ink-faint);
  font-style: italic;
}
.drift-update-btn {
  margin-left: auto;
}
.drift-diff {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.drift-diff-col {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.drift-diff-label {
  font-size: 11px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: .05em;
  margin: 0;
}
.drift-diff-old .drift-diff-label { color: #b91c1c; }
.drift-diff-new .drift-diff-label { color: #15803d; }
.drift-diff-body {
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  font-size: 11px;
  white-space: pre-wrap;
  word-break: break-all;
  margin: 0;
  padding: 8px 10px;
  border-radius: var(--r-sm);
  border: 1px solid var(--line);
  background: var(--paper);
  min-height: 60px;
}
.drift-diff-old .drift-diff-body { background: #fef2f2; border-color: #fca5a5; }
.drift-diff-new .drift-diff-body { background: #f0fdf4; border-color: #86efac; }

/* Single-rule editor (Feature 3) */
.single-rule-edit-entry {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 0;
}
.single-rule-edit-select {
  padding: 6px 10px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: var(--surface);
  font-size: 13px;
  color: var(--ink);
  min-width: 380px;
  cursor: pointer;
}
.single-rule-editor-overlay {
  position: fixed;
  inset: 0;
  background: rgba(27, 26, 24, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 300;
}
.single-rule-editor {
  background: var(--surface);
  border-radius: var(--r-lg);
  box-shadow: var(--shadow-pop);
  width: min(680px, 90vw);
  max-height: 80vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.single-rule-editor-head {
  padding: 18px 20px 14px;
  border-bottom: 1px solid var(--line);
}
.single-rule-editor-id-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 6px;
}
.single-rule-editor-body {
  padding: 16px 20px;
  overflow-y: auto;
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.single-rule-editor-actions {
  padding: 12px 20px;
  border-top: 1px solid var(--line);
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}
.single-rule-scope {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.single-rule-scope-btns {
  display: flex;
  gap: 8px;
}
.scope-btn {
  padding: 6px 14px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: var(--surface);
  font-size: 13px;
  cursor: pointer;
  color: var(--ink);
  transition: all 150ms var(--ease);
}
.scope-btn:hover { border-color: var(--accent); }
.scope-btn.active {
  background: var(--accent);
  color: #fff;
  border-color: var(--accent);
  font-weight: 600;
}
.single-rule-repo-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 6px;
}
.single-rule-repo-label {
  font-size: 13px;
  font-weight: 600;
  color: var(--ink);
  min-width: 90px;
}
.single-rule-options {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

/* Deep-report export panel (Feature 4) */
.deep-export-panel {
  padding: 14px 16px;
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  background: var(--surface);
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 16px;
}
.deep-export-modal-overlay {
  position: fixed;
  inset: 0;
  background: rgba(27, 26, 24, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 300;
}
.deep-export-modal {
  background: var(--surface);
  border-radius: var(--r-lg);
  box-shadow: var(--shadow-pop);
  width: min(760px, 90vw);
  max-height: 80vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 20px;
  gap: 12px;
}
.deep-export-modal-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.deep-export-modal-title {
  font-size: 15px;
  font-weight: 700;
  color: var(--ink);
  margin: 0;
}
.deep-export-body {
  flex: 1;
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  font-size: 12px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 12px;
  resize: none;
  background: var(--paper);
  color: var(--ink);
  min-height: 400px;
  overflow: auto;
}

/* Feature 5: SOC-2 disabled notice */
.deep-soc2-disabled-notice {
  font-size: 13px;
  color: #6b7280;
  font-style: italic;
  padding: 8px 12px;
  background: var(--line-soft);
  border-radius: var(--r-sm);
  border-left: 3px solid #d1d5db;
  margin: 0;
}

/* ── Governed Development page ──────────────────────────────────────────────
   Two-pane layout: a left nav (Issue Management entry + a card per Unit of Work)
   and a main area that shows the issue-management panel or a UoW's dev controls. */
.govdev { display: grid; grid-template-columns: 260px 1fr; min-height: 0; gap: 0; height: 100%; }
.govdev-nav {
  border-right: 1px solid var(--line); background: var(--rail-tint); padding: 14px;
  display: flex; flex-direction: column; gap: 6px; overflow-y: auto;
  height: 100%; min-height: 0;
}
.govdev-nav-top {
  display: flex; flex-direction: column; gap: 2px; align-items: flex-start; text-align: left;
  border: 1px solid var(--line); background: var(--surface); border-radius: 9px;
  padding: 11px 12px; cursor: pointer;
  transition: border-color .15s var(--ease), box-shadow .15s var(--ease);
}
.govdev-nav-top:hover { border-color: var(--ink-faint); }
.govdev-nav-top.on { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-wash); }
.govdev-nav-top-title { font-size: 13.5px; font-weight: 700; color: var(--ink); }
.govdev-nav-top-sub { font-size: 11px; color: var(--ink-faint); }
.govdev-nav-label {
  font-size: 10px; font-weight: 800; letter-spacing: .07em; text-transform: uppercase;
  color: var(--ink-faint); margin: 14px 0 4px;
}
.govdev-uow-list { display: flex; flex-direction: column; gap: 6px; }
.govdev-uow-empty { font-size: 12px; color: var(--ink-faint); font-style: italic; margin: 0; }
.govdev-uow-card {
  display: flex; flex-direction: row; gap: 6px; align-items: center; text-align: left;
  border: 1px solid var(--line); background: var(--surface); border-radius: 9px;
  padding: 9px 10px;
  transition: border-color .15s var(--ease), box-shadow .15s var(--ease);
}
.govdev-uow-card:hover { border-color: var(--ink-faint); }
.govdev-uow-card.sel { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-wash); }
.govdev-uow-cardmain { display: flex; flex-direction: column; gap: 6px; align-items: flex-start; flex: 1; min-width: 0; cursor: pointer; }
.govdev-uow-trash { flex: none; background: none; border: none; cursor: pointer; opacity: .45; font-size: 13px; line-height: 1; padding: 4px 6px; border-radius: 6px; }
.govdev-uow-trash:hover { opacity: 1; background: #fee2e2; }
.govdev-uow-confirm { flex: none; display: flex; align-items: center; gap: 4px; flex-wrap: wrap; }
.govdev-uow-confirm-q { font-size: 10.5px; color: #b91c1c; }
.govdev-uow-confirm-yes { font-size: 10.5px; background: #dc2626; color: #fff; border: none; border-radius: 5px; padding: 3px 7px; cursor: pointer; }
.govdev-uow-confirm-no { font-size: 10.5px; background: var(--surface); color: var(--ink); border: 1px solid var(--line); border-radius: 5px; padding: 3px 7px; cursor: pointer; }
.govdev-uow-title { font-size: 13px; font-weight: 600; color: var(--ink); line-height: 1.3; }
.govdev-uow-meta { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.govdev-uow-repo {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 10.5px;
  color: var(--ink-soft); background: var(--paper); border: 1px solid var(--line);
  padding: 1px 6px; border-radius: 5px;
}
.govdev-uow-stage {
  font-size: 10px; font-weight: 700; letter-spacing: .03em; color: var(--accent-ink);
  background: var(--accent-wash); border: 1px solid #e5c9bd; padding: 1px 7px; border-radius: 5px;
}
/* Transparent: this sits INSIDE .cockpit-scroll, which already carries the single
   --page-tint. Tinting here too would double-stack and make gov-dev read denser than
   every other page. Letting the .cockpit-scroll tint show through keeps it identical. */
.govdev-main { padding: 18px 22px; overflow-y: auto; min-width: 0; background: transparent; }
.govdev-h { font-size: 17px; font-weight: 700; color: var(--ink); margin: 0 0 14px; }

/* Gear-button row at the top of the govdev left nav. */
.govdev-gear-row { display: flex; justify-content: flex-end; margin-bottom: 6px; }
.govdev-gear-btn { font-size: 12px; gap: 4px; }

/* Project-settings popup (wider than a rule modal; holds tier-map editor). */
.proj-settings-modal { max-width: 560px; }
.proj-settings-scope-note {
  font-size: 12px; color: var(--ink-soft); margin: 6px 0 16px; font-style: italic;
}
.proj-settings-section { margin-top: 16px; padding-top: 16px; border-top: 1px solid var(--line); }

/* Issue Management panel */
.issue-mgmt { max-width: 1000px; }
.issue-conn {
  border: 1px solid var(--line); border-radius: 10px; background: var(--surface);
  padding: 12px 14px; margin-bottom: 14px; display: flex; flex-direction: column; gap: 8px;
}
.issue-conn-line { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.issue-conn-label { font-size: 11.5px; font-weight: 600; color: var(--ink-soft); min-width: 96px; }
.issue-conn-prov { font-size: 13px; font-weight: 700; color: var(--ink); }
.issue-conn-repos {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink);
}
.issue-conn-none { font-size: 12px; color: var(--ink-faint); font-style: italic; }
.issue-pull-row { display: flex; align-items: center; gap: 12px; margin-bottom: 16px; flex-wrap: wrap; }

/* Work-item table (provider-agnostic) */
.wi-table { width: 100%; border-collapse: collapse; border: 1px solid var(--line); border-radius: 10px; overflow: hidden; }
.wi-table thead th {
  text-align: left; font-size: 10.5px; font-weight: 800; letter-spacing: .05em; text-transform: uppercase;
  color: var(--ink-faint); padding: 9px 11px; background: var(--paper); border-bottom: 1px solid var(--line);
}
.wi-row { cursor: pointer; transition: background .12s var(--ease); border-bottom: 1px solid var(--line-soft); }
.wi-row:hover { background: var(--accent-wash); }
.wi-row td { padding: 9px 11px; font-size: 13px; color: var(--ink); vertical-align: middle; }
.wi-col-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; color: var(--ink-soft); white-space: nowrap; }
.wi-col-num { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--ink-soft); white-space: nowrap; }
.wi-col-title { font-weight: 600; }
.wi-col-labels { font-size: 11.5px; color: var(--ink-soft); }
.wi-col-act { text-align: right; white-space: nowrap; }
.wi-state {
  font-size: 9.5px; font-weight: 700; letter-spacing: .04em; padding: 2px 7px; border-radius: 5px;
  text-transform: uppercase;
}
.wi-state.active  { background: rgba(47,95,158,0.20); color: #7ca8e0; }
.wi-state.done    { background: rgba(22,163,74,0.18); color: #4ade80; }
.wi-state.neutral { background: rgba(140,128,117,0.18); color: var(--ink-soft); }

/* Work-item detail */
.wi-detail {
  margin-top: 16px; border: 1px solid var(--line); border-radius: 10px; background: rgba(26,24,22,0.55);
  padding: 14px 16px;
}
.wi-detail-head { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; margin-bottom: 8px; }
.wi-detail-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink-soft); }
.wi-detail-num { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink-faint); }
.wi-detail-close { margin-left: auto; border: 1px solid var(--line); background: var(--paper); color: var(--ink-soft); font-size: 12px; padding: 3px 10px; border-radius: 6px; cursor: pointer; }
.wi-detail-close:hover { color: var(--ink); border-color: var(--ink-faint); }
.wi-detail-title { font-size: 15.5px; font-weight: 700; color: var(--ink); margin: 0 0 8px; }
.wi-detail-body { font-size: 13px; color: var(--ink-soft); line-height: 1.5; white-space: pre-wrap; margin: 0 0 10px; }
.wi-detail-body.empty { font-style: italic; color: var(--ink-faint); }
.wi-detail-link { font-size: 12.5px; font-weight: 600; color: var(--accent-ink); text-decoration: none; }
.wi-detail-link:hover { text-decoration: underline; }
.wi-detail-actions { margin-top: 12px; }

/* UoW dev controls */
.uow-dev { max-width: 1400px; }
.uow-dev-head { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; margin-bottom: 4px; }
.uow-dev-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink-soft); }
.uow-dev-num { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; color: var(--ink-faint); }
.uow-dev-title { font-size: 17px; font-weight: 700; color: var(--ink); margin: 0 0 14px; }
.uow-dev-pull-row { display: flex; align-items: center; gap: 12px; margin-bottom: 14px; flex-wrap: wrap; }
.uow-comment { margin-top: 16px; border: 1px solid var(--line); border-radius: 10px; background: rgba(26,24,22,0.55); padding: 14px 16px; }
/* The Post-comment button sits below the composer with breathing room (TASK 2). */
.uow-comment .btn-run { margin-top: 12px; }

/* ── AI story authoring panel (2026-06-22) ─────────────────────────────────── */
.uow-dev-section-h { font-size: 13px; font-weight: 700; color: var(--ink); margin: 18px 0 8px; }
.authoring-chat {
  display: flex; flex-direction: column; gap: 10px;
  border: 1px solid var(--line); border-radius: 10px; background: var(--surface);
  padding: 14px 16px; max-height: 360px; overflow-y: auto;
}
.authoring-msg { display: flex; flex-direction: column; gap: 3px; max-width: 90%; }
.authoring-msg.user { align-self: flex-end; align-items: flex-end; }
.authoring-msg.ai { align-self: flex-start; align-items: flex-start; }
.authoring-msg-role { font-size: 10.5px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; color: var(--ink-faint); }
.authoring-msg-text {
  margin: 0; font-size: 13.5px; line-height: 1.5; color: var(--ink); white-space: pre-wrap;
  border: 1px solid var(--line); border-radius: 10px; padding: 8px 11px; background: var(--paper);
}
.authoring-msg.user .authoring-msg-text { background: var(--accent-wash); border-color: #e5c9bd; }
.authoring-input-row { display: flex; gap: 10px; align-items: flex-end; margin-top: 10px; }
/* Active-run Stop row: button + Bombe spinner + hint, aligned on one baseline. */
.uow-run-stop-row { display: flex; gap: 10px; align-items: center; margin: 8px 0; }
.authoring-input {
  flex: 1; resize: vertical; font: inherit; font-size: 13.5px; line-height: 1.5; color: var(--ink);
  border: 1px solid var(--line); border-radius: 10px; background: var(--paper); padding: 9px 11px;
}
.authoring-preview {
  margin-top: 16px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); padding: 14px 16px;
}
.authoring-publish {
  margin-top: 16px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); padding: 14px 16px;
}
.authoring-publish-row { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.authoring-repo-label { font-size: 12.5px; font-weight: 600; color: var(--ink-soft); }
.authoring-repo-select {
  font: inherit; font-size: 13px; color: var(--ink); border: 1px solid var(--line);
  border-radius: 8px; background: var(--paper); padding: 7px 10px;
}

/* @-mention autocomplete: the composer wrapper is the positioning context so the
   dropdown anchors to the textarea. */
.uow-comment-box { position: relative; }
.uow-mention-dropdown {
  position: absolute; left: 0; right: 0; top: calc(100% + 2px); z-index: 20;
  display: flex; flex-direction: column;
  border: 1px solid var(--line); border-radius: 8px; background: var(--paper);
  box-shadow: 0 6px 18px rgba(0,0,0,.12); overflow: hidden; max-height: 220px; overflow-y: auto;
}
.uow-mention-option {
  text-align: left; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12.5px; color: var(--ink); background: transparent; border: 0;
  padding: 7px 11px; cursor: pointer; border-bottom: 1px solid var(--line-soft);
}
.uow-mention-option:last-child { border-bottom: 0; }
.uow-mention-option:hover { background: var(--accent-wash); }

/* Work-item comments thread inside the detail modal. */
.wi-comments { margin-top: 16px; border-top: 1px solid var(--line); padding-top: 12px; }
.wi-comments-h { font-size: 12px; font-weight: 800; letter-spacing: .04em; text-transform: uppercase; color: var(--ink-faint); margin: 0 0 10px; }
.wi-comments-empty { font-style: italic; }
.wi-comment { border: 1px solid var(--line-soft); border-radius: 8px; background: rgba(26,24,22,0.5); padding: 9px 11px; margin-bottom: 8px; }
.wi-comment:last-child { margin-bottom: 0; }
.wi-comment-meta { display: flex; align-items: center; gap: 10px; margin-bottom: 5px; }
.wi-comment-author { font-size: 12.5px; font-weight: 700; color: var(--ink); }
.wi-comment-date { font-size: 11px; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.wi-comment-body { font-size: 13px; color: var(--ink-soft); line-height: 1.5; }
.wi-comment-body.empty { font-style: italic; color: var(--ink-faint); }

/* =====================================================================
   BombeBg — full Bletchley Bombe machine background layer.
   Ported from docs/plans/camerata_ui_mockup.html (Bletchley !important
   overrides win; base values below match the effective rendered result).
   ===================================================================== */

/* ── Obscuring overlay: sits between the bombe (z-index 0) and the app
   shell (z-index 10+) at z-index 2.  pointer-events:none — never blocks
   clicks.  Idle: strong dark fill so the bombe is visible but very subtle
   and the app content is easy to read.  Running: overlay lightens so the
   bombe glows through more clearly without making text unreadable.  ── */
.bombe-overlay {
  position: fixed;
  inset: 0;
  z-index: 2;
  pointer-events: none;
  background: rgba(16, 13, 11, var(--bombe-overlay-idle-alpha));
  transition: background 0.6s ease;
}
.bombe-overlay.bombe-overlay-running {
  background: rgba(16, 13, 11, var(--bombe-overlay-run-alpha));
}

/* ── Outer wrapper: fixed full-viewport layer, below the app shell ── */
.bombe-bg-machine {
  position: fixed;
  inset: 0;
  z-index: 0;
  overflow: hidden;
  display: flex;
  justify-content: center;
  align-items: center;
  pointer-events: none;
  opacity: 0.5;        /* Bletchley raised value */
  /* Idle: dimmed + drained of colour so the machine reads as "powered down". The .bombe-running
     state (below) brightens + re-saturates it. The transitions make the machine LIGHT UP and GO
     DIM smoothly (powering on/off) instead of snapping on with the spin. */
  filter: brightness(0.72) saturate(0.5);
  transition: opacity 0.8s ease, filter 0.9s ease;
}
.bombe-bg-machine.bombe-running {
  opacity: 0.72;       /* Bletchley raised value */
  filter: brightness(1.06) saturate(1.1);
}

/* ── Cabinet body ── */
.bombe-cabinet {
  width: 1400px;
  height: 750px;
  background-color: #121110;
  /* Vertical steel-beam column dividers */
  background-image:
    linear-gradient(90deg,
      transparent 0px, transparent 180px,
      #2e2a25 180px, #443f38 183px, #2e2a25 186px, #1a1918 190px,
      transparent 190px, transparent 1160px,
      #2e2a25 1160px, #443f38 1163px, #2e2a25 1166px, #1a1918 1170px,
      transparent 1170px
    );
  border: 8px solid #292522;
  border-radius: 8px;
  box-shadow:
    0 35px 90px rgba(0,0,0,0.95),
    inset 0 0 120px rgba(0,0,0,0.95);
  display: grid;
  grid-template-columns: 180px 1fr 180px;
  padding: 24px;
  position: relative;
  contain: layout paint;
}

/* Woven copper wire loom running along the cabinet top */
.bombe-cabinet::after {
  content: '';
  position: absolute;
  top: -16px; left: 30px; right: 30px; height: 16px;
  background:
    repeating-linear-gradient(90deg,
      transparent 0px, transparent 60px,
      #eae3db 60px, #eae3db 63px,
      transparent 63px, transparent 120px),   /* white string ties */
    linear-gradient(to bottom,
      #ea580c 0px,  #ea580c 2px,
      #c2410c 2px,  #c2410c 4px,
      #dc2626 4px,  #dc2626 6px,
      #991b1b 6px,  #991b1b 8px,
      #ea580c 8px,  #ea580c 10px,
      #7f1d1d 10px
    );
  border-radius: 6px;
  opacity: 0.95;
  box-shadow:
    0 4px 8px rgba(0,0,0,0.6),
    inset 0 1px 0 rgba(255,255,255,0.2),
    inset 0 -1px 2px rgba(0,0,0,0.4);
}
/* Glowing loom when running */
.bombe-running .bombe-cabinet::after {
  box-shadow:
    0 0 20px rgba(239,68,68,0.85),
    0 4px 10px rgba(0,0,0,0.6);
}

/* ── Control panels (left / right) ── */
.bombe-panel {
  display: flex;
  flex-direction: column;
  gap: 20px;
  align-items: center;
  padding-top: 20px;
  background-color: #1a1917;
  /* Rivet dots at the four corners */
  background-image:
    radial-gradient(circle at 10px 10px,         #78716c 1.5px, #3c3b37 3px, transparent 4px),
    radial-gradient(circle at calc(100% - 10px) 10px, #78716c 1.5px, #3c3b37 3px, transparent 4px),
    radial-gradient(circle at 10px calc(100% - 10px), #78716c 1.5px, #3c3b37 3px, transparent 4px),
    radial-gradient(circle at calc(100% - 10px) calc(100% - 10px), #78716c 1.5px, #3c3b37 3px, transparent 4px);
  border: 3px solid #2d2925;
  border-radius: 6px;
  box-shadow: inset 0 2px 10px rgba(0,0,0,0.9);
}
.bombe-panel-label {
  font-weight: 800;
  font-size: 10px;
  color: #8c8075;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  letter-spacing: .06em;
}

/* ── Dial gauges ── */
.bombe-gauge {
  width: 54px; height: 54px;
  border-radius: 50%;
  background: radial-gradient(circle, #fbf7ee 0%, #e5d9bd 75%, #c5b694 100%);
  border: 3.5px solid #2e2b27;
  position: relative;
  box-shadow: inset 0 3px 6px rgba(0,0,0,0.4), 0 2px 4px rgba(0,0,0,0.6);
}
/* Tick-mark ring (semi-transparent conic overlay, masked to edge only) */
.bombe-gauge::before {
  content: '';
  position: absolute;
  top: 2px; left: 2px; right: 2px; bottom: 2px;
  border-radius: 50%;
  background: repeating-conic-gradient(from 220deg, #2e2b27 0deg 2deg, transparent 2deg 20deg);
  -webkit-mask-image: radial-gradient(circle, transparent 70%, black 75%);
  mask-image: radial-gradient(circle, transparent 70%, black 75%);
  opacity: 0.55;
}
.bombe-needle {
  position: absolute;
  bottom: 50%; left: 50%;
  width: 2px; height: 20px;
  background: #b91c1c;
  transform-origin: bottom center;
  transform: rotate(-45deg);
  transition: transform 0.15s ease-out;
}
.bombe-running .bombe-needle {
  animation: gauge-vibe 0.3s infinite alternate;
}
@keyframes gauge-vibe {
  0%   { transform: rotate(10deg); }
  50%  { transform: rotate(20deg); }
  100% { transform: rotate(15deg); }
}

/* ── Cable bundles (vertical looms) ── */
.bombe-cable-bundle {
  width: 14px;
  flex: 1;
  background:
    repeating-linear-gradient(180deg,
      transparent 0px, transparent 40px,
      #eae3db 40px, #eae3db 42px,
      transparent 42px, transparent 80px),   /* white string ties */
    linear-gradient(to right, #ea580c 0px, #dc2626 4px, #7f1d1d 12px);
  border-radius: 4px;
  box-shadow: 3px 0 8px rgba(0,0,0,0.6), inset -1px 0 2px rgba(0,0,0,0.4);
}
.bombe-cable-bundle.right-cables {
  background:
    repeating-linear-gradient(180deg,
      transparent 0px, transparent 40px,
      #eae3db 40px, #eae3db 42px,
      transparent 42px, transparent 80px),
    linear-gradient(to right, #eab308 0px, #ca8a04 4px, #854d0e 12px);
}

/* ── Rotor matrix container ── */
.bombe-rotors-matrix {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 0 16px;
  justify-content: center;
}

/* ── Block backboard (plated dark bakelite) ── */
.bombe-block {
  display: grid;
  grid-template-columns: repeat(12, 1fr);
  gap: 8px;
  padding: 10px;
  background: #151413;
  border: 2px solid #272421;
  border-radius: 6px;
  box-shadow: inset 0 4px 15px rgba(0,0,0,0.9);
}

/* ── Rotor socket ── */
.bg-bombe-rotor {
  width: 58px; height: 58px;
  border-radius: 50%;
  background: #090807;
  border: 3px solid #25221e;
  position: relative;
  box-shadow: inset 0 3px 8px rgba(0,0,0,0.9), 0 2px 4px rgba(0,0,0,0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  contain: layout paint;
  /* Outer contact pins (26 metallic slits around the rim) */
}
/* Gilded/amber glow when running */
.bombe-running .bg-bombe-rotor {
  border-color: rgba(245,158,11,0.8);
  box-shadow:
    0 0 25px rgba(245,158,11,0.65),
    0 0 10px rgba(245,158,11,0.3),
    inset 0 0 12px rgba(245,158,11,0.25);
}

/* ── Rotor drum (the spinning element — ONE child per socket) ─────────────
   The drum carries all the visual layers as layered backgrounds so no extra
   child elements are needed.  It spins via rotor-clicking-spin when running.
   ── */
.rotor-drum {
  width: 100%; height: 100%;
  border-radius: 50%;
  position: relative;
  will-change: transform;
  /* Layer 1 (topmost): pointer — a thin amber bar at 12 o'clock */
  /* Layer 2: centre hub — a small metallic disc */
  /* Layer 3: outer contacts ring — 26 evenly-spaced white slits */
  /* Layer 4: bakelite base disc (colour comes from row-class below) */
  /* The static initial rotation from --start-angle is applied inline. */
  transform: rotate(var(--start-angle, 0deg));
}

/* ── Drum layer: outer contact-pin ring (26 metallic slits) ── */
.rotor-drum::before {
  content: '';
  position: absolute;
  inset: 0;
  border-radius: 50%;
  background: repeating-conic-gradient(
    from 0deg,
    rgba(255,255,255,0.65) 0deg 2.2deg,
    transparent 2.2deg 13.85deg
  );
  -webkit-mask-image: radial-gradient(circle, transparent 86%, black 88%);
  mask-image: radial-gradient(circle, transparent 86%, black 88%);
  pointer-events: none;
}

/* ── Drum layer: 12 o'clock pointer (amber) — drawn as an after pseudo ── */
.rotor-drum::after {
  content: '';
  position: absolute;
  top: 5px; left: 50%;
  width: 2px; height: 18px;
  margin-left: -1px;
  border-radius: 1px;
  pointer-events: none;
}
.bombe-running .rotor-drum::after {
  box-shadow: 0 0 12px #f97316;
}

/* ── Row-specific bakelite disc colour (backgrounds on the drum itself) ── */
/* Top row: reddish-brown bakelite, brass pointer */
.bombe-row-top .rotor-drum {
  background:
    /* pointer bar */
    linear-gradient(to bottom, #ca8a04 0%, #a16207 100%)
      no-repeat center top / 2px 18px,
    /* centre hub metallic disc */
    radial-gradient(circle at 35% 35%, #fff 0%, #cbd5e1 55%, #64748b 100%)
      no-repeat center / 18px 18px,
    /* bakelite disc */
    radial-gradient(circle, #732218 0%, #400f08 100%)
      no-repeat center / calc(100% - 6px) calc(100% - 6px),
    /* outer socket (background of the drum itself is the socket colour) */
    transparent;
}
/* Middle row: cream/yellowish bakelite, dark-brown pointer */
.bombe-row-mid .rotor-drum {
  background:
    linear-gradient(to bottom, #451a03 0%, #1c0a00 100%)
      no-repeat center top / 2px 18px,
    radial-gradient(circle at 35% 35%, #fff 0%, #cbd5e1 55%, #64748b 100%)
      no-repeat center / 18px 18px,
    radial-gradient(circle, #ecdcb7 0%, #b59f6b 100%)
      no-repeat center / calc(100% - 6px) calc(100% - 6px),
    transparent;
}
/* Bottom row: crimson bakelite, brass pointer */
.bombe-row-bot .rotor-drum {
  background:
    linear-gradient(to bottom, #ca8a04 0%, #a16207 100%)
      no-repeat center top / 2px 18px,
    radial-gradient(circle at 35% 35%, #fff 0%, #cbd5e1 55%, #64748b 100%)
      no-repeat center / 18px 18px,
    radial-gradient(circle, #7e1e12 0%, #450c05 100%)
      no-repeat center / calc(100% - 6px) calc(100% - 6px),
    transparent;
}

/* ── Rotor spin animation ──
   The animation is ALWAYS applied but PAUSED when idle via animation-play-state. CSS preserves an
   animation's progress across pause/resume, so each rotor FREEZES at its current angle when
   processing stops and RESUMES from that exact position when it starts again — the knob positions
   persist across run/stop cycles instead of snapping back to the start angle. .bombe-running only
   flips the play-state to running. */
@keyframes rotor-clicking-spin {
  from { transform: rotate(var(--start-angle, 0deg)); }
  to   { transform: rotate(calc(var(--start-angle, 0deg) + 360deg)); }
}
.rotor-drum {
  animation-name: rotor-clicking-spin;
  animation-timing-function: steps(26, end);
  animation-iteration-count: infinite;
  animation-play-state: paused;
  /* animation-duration is set inline (0.9s / 26s / 78s per row) */
}
.bombe-running .rotor-drum {
  animation-play-state: running;
}

/* ── Status LEDs ── */
.bombe-status-leds {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.bombe-led-bulb {
  width: 12px; height: 12px;
  border-radius: 50%;
  background: #3f1712;
  border: 2.5px solid #6b5a3e;
  box-shadow: inset 0 1px 3px rgba(0,0,0,0.8);
  transition: all 0.3s;
}
.bombe-led-bulb.active {
  background: #f97316;
  border-color: #8f7956;
  box-shadow:
    0 0 15px #f97316,
    0 0 5px #f97316,
    inset 0 1px 0 rgba(255,255,255,0.4);
}
/* LED flicker keyframe (missing from the mockup — defined here as an opacity pulse) */
@keyframes led-flicker {
  0%   { opacity: 0.65; }
  40%  { opacity: 1.0; }
  70%  { opacity: 0.80; }
  100% { opacity: 0.95; }
}
.bombe-running .bombe-led-bulb:nth-child(1) {
  animation: led-flicker 0.6s infinite alternate;
}
.bombe-running .bombe-led-bulb:nth-child(2) {
  animation: led-flicker 0.4s infinite alternate-reverse 0.2s;
}
.bombe-running .bombe-led-bulb:nth-child(3) {
  animation: led-flicker 0.8s infinite alternate 0.4s;
}

/* ── App shell sits above both the bombe layer (z-index 0) and the
   obscuring overlay (z-index 2), so app content is always readable ── */
.app-root {
  position: relative;
  z-index: 10;
}

/* ═══════════════════════════════════════════════════════════════════════════
   BLETCHLEY COMPONENT THEME — ports the effective mockup CSS onto the app's
   actual class names.  Palette vars are already in :root above; this section
   adds per-component styling that was missing.  The trailing .app-root block
   below is intentionally omitted here (it lives above this comment block).
   ═══════════════════════════════════════════════════════════════════════════ */

/* ── Buttons ──────────────────────────────────────────────────────────────
   The mockup's Bletchley override turns all .btn variants into bakelite /
   cast-iron panels with a bevel border and glossy ::after highlight.
   The app uses: .btn-run (primary amber), .btn-secondary, .btn-stop,
   .btn-restart, .btn-run-sm, .btn-edit-sm, .btn-delete-sm.
   We also introduce .btn-solid-primary / .btn-solid-success / .btn-solid-danger
   / .btn-outline / .btn-danger-outline that map to the mockup's named variants.
   ─────────────────────────────────────────────────────────────────────── */

/* Shared aero geometry: all action buttons get overflow:hidden so ::after
   can clip cleanly, and position:relative for the pseudo-element stacking. */
.btn-run, .btn-secondary, .btn-stop, .btn-restart,
.btn-run-sm, .btn-edit-sm, .btn-delete-sm,
.btn-solid-primary, .btn-solid-success, .btn-solid-danger,
.btn-outline, .btn-danger-outline, .onboard-cta {
  position: relative;
  overflow: hidden;
}

/* Top-highlight pseudo-element (glossy aero sheen) */
.btn-run::after, .btn-secondary::after, .btn-stop::after, .btn-restart::after,
.btn-run-sm::after, .btn-edit-sm::after, .btn-delete-sm::after,
.btn-solid-primary::after, .btn-solid-success::after, .btn-solid-danger::after,
.btn-outline::after, .btn-danger-outline::after, .onboard-cta::after {
  content: '';
  position: absolute;
  top: 1px; left: 1px; right: 1px;
  height: 35%;
  background: linear-gradient(to bottom, rgba(255,255,255,0.10) 0%, rgba(255,255,255,0) 100%);
  border-radius: 3px 3px 0 0;
  pointer-events: none;
}

/* Primary amber run button — the Bletchley industrial look */
.btn-run {
  background: linear-gradient(180deg, var(--accent) 0%, var(--accent-ink) 100%);
  border: 2px solid var(--accent-ink);
  border-top-color: var(--accent);
  border-bottom-color: #7a5200;
  box-shadow: 0 3px 8px rgba(0,0,0,0.45), inset 0 1px 0 rgba(255,255,255,0.12);
  text-shadow: 0 1px 2px rgba(0,0,0,0.5);
}
.btn-run:hover:not(:disabled) {
  background: linear-gradient(180deg, #dba012 0%, var(--accent-ink) 100%);
  box-shadow: 0 5px 14px rgba(0,0,0,0.5), inset 0 1px 0 rgba(255,255,255,0.14);
}
.btn-run:active:not(:disabled) {
  background: linear-gradient(180deg, #a16207 0%, #7a5200 100%);
  border-top-color: #7a5200;
  border-bottom-color: var(--accent-ink);
  transform: translateY(1px);
  box-shadow: 0 1px 3px rgba(0,0,0,0.4), inset 0 2px 5px rgba(0,0,0,0.5);
}

/* Secondary bordered button */
.btn-secondary {
  background: linear-gradient(180deg, #2a2825 0%, #1c1a18 100%);
  border: 2px solid #3a3530;
  border-top-color: #4a4540;
  border-bottom-color: #0c0b0a;
  color: var(--ink);
  box-shadow: 0 2px 6px rgba(0,0,0,0.45), inset 0 1px 0 rgba(255,255,255,0.06);
}
.btn-secondary:hover:not(:disabled) {
  background: linear-gradient(180deg, #333028 0%, #242220 100%);
  border-color: var(--accent-ink);
  color: var(--accent-ink);
  box-shadow: 0 4px 10px rgba(0,0,0,0.5), inset 0 1px 0 rgba(255,255,255,0.08);
}
.btn-secondary.danger:hover:not(:disabled) {
  border-color: var(--danger-color);
  color: var(--danger-color);
}

/* Stop / cancel button */
.btn-stop {
  background: linear-gradient(180deg, #2a2825 0%, #1c1a18 100%);
  border: 2px solid rgba(185,28,28,0.5);
  border-top-color: rgba(220,38,38,0.5);
  border-bottom-color: rgba(100,10,10,0.7);
  color: #f87171;
  box-shadow: 0 2px 6px rgba(0,0,0,0.45);
}
.btn-stop:hover:not(:disabled) {
  border-color: var(--danger-color);
  color: #ef4444;
  background: linear-gradient(180deg, #2f1a1a 0%, #1f1010 100%);
}

/* Small restart / "quiet action" button */
.btn-restart {
  background: linear-gradient(180deg, #2a2825 0%, #1c1a18 100%);
  border: 2px solid #3a3530;
  border-top-color: #4a4540;
  border-bottom-color: #0c0b0a;
  color: var(--ink-soft);
  font-size: 12px;
  font-weight: 600;
  padding: 5px 12px;
  border-radius: 7px;
  box-shadow: 0 2px 5px rgba(0,0,0,0.4), inset 0 1px 0 rgba(255,255,255,0.05);
}
.btn-restart:hover {
  border-color: var(--accent-ink);
  color: var(--accent-ink);
  background: linear-gradient(180deg, #333028 0%, #242220 100%);
}

/* Small run button variant */
.btn-run-sm {
  background: linear-gradient(180deg, var(--accent) 0%, var(--accent-ink) 100%);
  border: 2px solid var(--accent-ink);
  border-top-color: var(--accent);
  border-bottom-color: #7a5200;
  box-shadow: 0 2px 5px rgba(0,0,0,0.4);
}
.btn-run-sm:hover { background: linear-gradient(180deg, #dba012 0%, var(--accent-ink) 100%); }

/* Small edit / inline action button */
.btn-edit-sm {
  background: linear-gradient(180deg, #2a2825 0%, #1c1a18 100%);
  border: 2px solid #3a3530;
  border-top-color: #4a4540;
  border-bottom-color: #0c0b0a;
  color: var(--ink);
  box-shadow: 0 2px 4px rgba(0,0,0,0.4);
}
.btn-edit-sm:hover {
  border-color: var(--accent-ink);
  color: var(--accent-ink);
  background: linear-gradient(180deg, #333028 0%, #242220 100%);
}

/* Small delete button */
.btn-delete-sm {
  background: linear-gradient(180deg, #2a2825 0%, #1c1a18 100%);
  border: 2px solid #3a3530;
  border-top-color: #4a4540;
  border-bottom-color: #0c0b0a;
  color: var(--ink-soft);
  box-shadow: 0 2px 4px rgba(0,0,0,0.4);
}
.btn-delete-sm:hover { border-color: var(--danger-color); color: var(--danger-color); }
.btn-delete-sm.confirm { background: linear-gradient(180deg, #b91c1c 0%, #7f1d1d 100%); border-color: #b91c1c; color: #fff; }

/* Named button variants (for new components and the Re-emit button) */
.btn-solid-primary {
  background: linear-gradient(180deg, var(--accent) 0%, var(--accent-ink) 100%);
  border: 2px solid var(--accent-ink);
  border-top-color: var(--accent);
  border-bottom-color: #7a5200;
  color: #fff;
  text-shadow: 0 1px 2px rgba(0,0,0,0.5);
  box-shadow: 0 3px 8px rgba(0,0,0,0.45), inset 0 1px 0 rgba(255,255,255,0.12);
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 7px; cursor: pointer;
}
.btn-solid-primary:hover:not(:disabled) {
  background: linear-gradient(180deg, #dba012 0%, var(--accent-ink) 100%);
  box-shadow: 0 5px 14px rgba(0,0,0,0.5);
}
.btn-solid-primary:active:not(:disabled) { transform: translateY(1px); box-shadow: 0 1px 3px rgba(0,0,0,0.4); }
.btn-solid-primary:disabled { opacity: 0.45; cursor: not-allowed; }

.btn-solid-success {
  background: linear-gradient(180deg, #16a34a 0%, #15803d 100%);
  border: 2px solid #166534;
  border-top-color: #22c55e;
  border-bottom-color: #14532d;
  color: #fff;
  text-shadow: 0 1px 2px rgba(0,0,0,0.4);
  box-shadow: 0 3px 8px rgba(0,0,0,0.4), inset 0 1px 0 rgba(255,255,255,0.12);
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 7px; cursor: pointer;
}
.btn-solid-success:hover:not(:disabled) {
  background: linear-gradient(180deg, #22c55e 0%, #16a34a 100%);
  box-shadow: 0 5px 12px rgba(22,163,74,0.35);
}

.btn-solid-danger {
  background: linear-gradient(180deg, #b91c1c 0%, #7f1d1d 100%);
  border: 2px solid #b91c1c;
  border-top-color: #dc2626;
  border-bottom-color: #450a0a;
  color: #fff;
  text-shadow: 0 1px 2px rgba(0,0,0,0.5);
  box-shadow: 0 3px 8px rgba(0,0,0,0.5), inset 0 1px 0 rgba(255,255,255,0.15);
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 7px; cursor: pointer;
}
.btn-solid-danger:hover:not(:disabled) {
  background: linear-gradient(180deg, #dc2626 0%, #991b1b 100%);
  box-shadow: 0 5px 14px rgba(185,28,28,0.45);
}

.btn-outline {
  background: linear-gradient(180deg, #2a2825 0%, #1c1a18 100%);
  border: 2px solid #3a3530;
  border-top-color: #4a4540;
  border-bottom-color: #0c0b0a;
  color: var(--ink-soft);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.06);
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 7px; cursor: pointer;
}
.btn-outline:hover:not(:disabled) {
  border-color: var(--accent-ink);
  color: var(--accent-ink);
  background: linear-gradient(180deg, #333028 0%, #242220 100%);
}

.btn-danger-outline {
  background: linear-gradient(180deg, #2a2825 0%, #1c1a18 100%);
  border: 2px solid rgba(185,28,28,0.4);
  border-top-color: rgba(220,38,38,0.5);
  border-bottom-color: rgba(80,10,10,0.6);
  color: #f87171;
  box-shadow: 0 2px 5px rgba(0,0,0,0.4);
  font-size: 13px; font-weight: 700; padding: 9px 16px; border-radius: 7px; cursor: pointer;
}
.btn-danger-outline:hover:not(:disabled) {
  border-color: var(--danger-color);
  color: #ef4444;
  background: linear-gradient(180deg, #2f1a1a 0%, #1f1010 100%);
}

/* ── Glass / card surfaces ───────────────────────────────────────────────
   The mockup adds "silver rivet" corner dots and a heavier cast-iron border
   to every card surface.  App classes: .pg-card, .card, .live-run, .sups-panel,
   .custom-rules, .audit-cost, .fix-panel, .uow-panel, .uow-step-control.
   ─────────────────────────────────────────────────────────────────────── */

/* Rivet pattern shared by all card surfaces */
.pg-card,
.live-run,
.sups-panel,
.audit-cost,
.fix-panel,
.uow-step-control {
  border-width: 2px;
  border-color: var(--line);
  box-shadow:
    0 12px 30px rgba(0,0,0,0.65),
    inset 0 1px 0 rgba(255,255,255,0.04),
    inset 0 -1px 3px rgba(0,0,0,0.45);
  background-image:
    radial-gradient(circle at 10px 10px, #57534e 2px, #2e2a25 3.5px, transparent 4.5px),
    radial-gradient(circle at calc(100% - 10px) 10px, #57534e 2px, #2e2a25 3.5px, transparent 4.5px),
    radial-gradient(circle at 10px calc(100% - 10px), #57534e 2px, #2e2a25 3.5px, transparent 4.5px),
    radial-gradient(circle at calc(100% - 10px) calc(100% - 10px), #57534e 2px, #2e2a25 3.5px, transparent 4.5px);
  background-repeat: no-repeat;
  background-size: 100% 100%;
}

/* Project-gate card gets a hover lift */
.pg-card:hover {
  border-color: var(--accent-ink);
  box-shadow:
    0 16px 40px rgba(0,0,0,0.75),
    inset 0 1px 0 rgba(255,255,255,0.06),
    0 0 0 1px var(--accent-ink);
  transform: translateY(-2px);
  transition: all 0.25s var(--ease);
}
.pg-card { transition: all 0.25s var(--ease); }

/* ── Forms (inputs, selects, textareas) ──────────────────────────────────
   The mockup themes all form controls as "terminal slot lines": inset shadow,
   near-black bg, copper-orange focus glow, monospace font.
   App uses: .addressee-input, .alt-select, .onboard-repos-input, the generic
   input / select / textarea elements in cockpit forms.
   ─────────────────────────────────────────────────────────────────────── */

input:not([type="checkbox"]):not([type="radio"]):not([type="range"]),
textarea,
select {
  background-color: #11100f;
  border: 2px solid var(--line);
  border-top-color: #0b0a0a;
  border-left-color: #0b0a0a;
  color: var(--ink);
  font-family: "Courier Prime", ui-monospace, SFMono-Regular, Menlo, monospace;
  border-radius: 4px;
  box-shadow: inset 0 2px 4px rgba(0,0,0,0.7);
  transition: border-color .2s var(--ease), box-shadow .2s var(--ease);
}
input:not([type="checkbox"]):not([type="radio"]):not([type="range"]):focus,
textarea:focus,
select:focus {
  border-color: var(--warning-color);
  outline: none;
  box-shadow: 0 0 5px rgba(234,88,12,0.35), inset 0 2px 4px rgba(0,0,0,0.7);
}

/* ── Global header ────────────────────────────────────────────────────────
   The cockpit's top bar / topbar maps to the app's .cockpit-topbar.
   The brand panel is .topbar-brand and nav tabs are .cockpit-nav-tab.
   ─────────────────────────────────────────────────────────────────────── */

.cockpit-topbar {
  background: #1b1a18;
  border-bottom: 3px solid var(--line);
  box-shadow: 0 4px 15px rgba(0,0,0,0.55);
  position: relative;
}
/* Red-orange cable harness running along header top */
.cockpit-topbar::before {
  content: '';
  position: absolute;
  top: 0; left: 0; right: 0; height: 4px;
  background: repeating-linear-gradient(
    90deg,
    #dc2626 0px, #dc2626 8px,
    #ea580c 8px, #ea580c 16px,
    #991b1b 16px, #991b1b 24px
  );
  box-shadow: 0 1px 3px rgba(0,0,0,0.45);
  pointer-events: none;
}

.topbar-brand {
  font-family: "Courier Prime", ui-monospace, monospace;
  font-weight: 700;
  color: var(--ink);
  letter-spacing: 0.03em;
}

/* ── Cockpit nav tabs ─────────────────────────────────────────────────────
   .cockpit-nav acts as the toolbar under the topbar.
   .cockpit-nav-tab are the individual clickable tabs.
   ─────────────────────────────────────────────────────────────────────── */

.cockpit-nav {
  background: #1a1917;
  border-bottom: 2px solid var(--line);
}

.cockpit-nav-tab {
  font-family: "Courier Prime", ui-monospace, monospace;
  font-weight: 700;
  color: var(--ink-soft);
  border-radius: 3px;
  transition: color .15s var(--ease), background .15s var(--ease), border-color .15s var(--ease);
  border-bottom: 3px solid transparent;
}
.cockpit-nav-tab:hover { color: var(--ink); background: rgba(255,255,255,0.04); }
.cockpit-nav-tab.on {
  color: var(--accent);
  border-bottom-color: var(--accent);
  background: rgba(0,0,0,0.2);
  box-shadow: none;
}

/* ── UoW sidebar (cockpit left pane) ──────────────────────────────────────
   The app's cockpit body is .cockpit-body with .cockpit-rail as the left
   sidebar.  .spine-item are the UoW story items.
   ─────────────────────────────────────────────────────────────────────── */

.cockpit-rail {
  background: var(--rail-tint);
  border-right: 2px solid var(--line);
}

.spine-item {
  background: rgba(0,0,0,0.18);
  border: 2px solid var(--line);
  border-radius: 4px;
  box-shadow: 0 2px 6px rgba(0,0,0,0.22);
  position: relative;
  transition: border-color .2s var(--ease), box-shadow .2s var(--ease), transform .2s var(--ease);
}
.spine-item::before {
  content: '';
  position: absolute;
  top: 0; left: 0; right: 0; height: 35%;
  background: linear-gradient(to bottom, rgba(255,255,255,0.04) 0%, rgba(255,255,255,0) 100%);
  pointer-events: none;
  border-radius: 3px 3px 0 0;
}
.spine-item:hover {
  border-color: var(--accent-ink);
  transform: translateY(-1px);
  box-shadow: 0 4px 12px rgba(0,0,0,0.35);
}
.spine-item.sel {
  background: linear-gradient(135deg, rgba(80,50,0,0.45) 0%, rgba(40,25,0,0.35) 100%);
  border-color: var(--accent);
  box-shadow: 0 4px 14px rgba(0,0,0,0.3), inset 0 1px 0 rgba(255,255,255,0.04);
}

/* UoW phase tags */
.spine-badge {
  font-family: "Courier Prime", ui-monospace, monospace;
  font-weight: 700;
  text-transform: uppercase;
  border-radius: 3px;
  box-shadow: 1px 1px 3px rgba(0,0,0,0.5);
  letter-spacing: 0.04em;
}
.spine-badge.neutral, .topbar-status.neutral { background: #2e2a25; color: var(--ink-soft); border: 1px solid var(--line); }
.spine-badge.active, .topbar-status.active   { background: rgba(202,138,4,0.18); color: var(--accent); border: 1px solid var(--accent-ink); }
.spine-badge.warn, .topbar-status.warn       { background: rgba(234,88,12,0.15); color: #fbbf24; border: 1px solid rgba(234,88,12,0.4); }
.spine-badge.done, .topbar-status.done       { background: rgba(22,163,74,0.15); color: #86efac; border: 1px solid rgba(22,163,74,0.35); }
.spine-badge.block, .topbar-status.block     { background: #3f0f0a; color: #fca5a5; border: 1px solid #b91c1c; }

/* ── Onboarding option cards ──────────────────────────────────────────────
   App uses .onboard-path cards for the "browse / quick" options.
   ─────────────────────────────────────────────────────────────────────── */

.onboard-path {
  background: var(--glass-bg);
  border: 2px solid var(--glass-border);
  border-radius: 5px;
  box-shadow: var(--glass-shadow);
  position: relative;
  transition: border-color .25s var(--ease), box-shadow .25s var(--ease), transform .25s var(--ease);
}
.onboard-path::before {
  content: '';
  position: absolute;
  top: 0; left: 0; right: 0; height: 35%;
  background: linear-gradient(to bottom, rgba(255,255,255,0.05) 0%, rgba(255,255,255,0) 100%);
  pointer-events: none;
  border-radius: 4px 4px 0 0;
}
.onboard-path:hover {
  border-color: var(--accent-ink);
  box-shadow: 0 8px 24px rgba(0,0,0,0.5);
  transform: translateY(-2px);
}
.onboard-path.on {
  border-color: var(--accent);
  background: linear-gradient(135deg, rgba(80,50,0,0.6) 0%, rgba(40,25,0,0.5) 100%);
  box-shadow: 0 10px 28px rgba(0,0,0,0.45), inset 0 1px 0 rgba(255,255,255,0.04);
}

/* ── Scanner progress bar ─────────────────────────────────────────────────
   App renders the scan progress inline; .scanner-progress-bar /
   .scanner-progress-fill re-use the mockup names exactly.
   ─────────────────────────────────────────────────────────────────────── */

.scanner-progress-bar {
  height: 8px;
  background: rgba(0,0,0,0.4);
  border-radius: 6px;
  overflow: hidden;
  margin: 12px 0;
  box-shadow: inset 0 1.5px 3px rgba(0,0,0,0.3);
  border: 1px solid var(--line);
}
.scanner-progress-fill {
  height: 100%;
  background: linear-gradient(90deg, var(--warning-color) 0%, var(--accent) 60%, var(--good) 100%);
  border-radius: 6px;
  box-shadow: 0 0 8px rgba(202,138,4,0.3);
  transition: width 0.3s ease-out;
  position: relative;
}
.scanner-progress-fill::after {
  content: '';
  position: absolute;
  top: 0; left: 0; right: 0;
  height: 40%;
  background: rgba(255,255,255,0.35);
}

/* ── Scan log panel ───────────────────────────────────────────────────────
   App renders scan logs as a scrolling text area; we give it the terminal
   slot look.
   ─────────────────────────────────────────────────────────────────────── */

.scan-logs {
  font-family: "Courier Prime", ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  color: var(--ink);
  max-height: 180px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: 14px;
  padding: 14px;
  background: rgba(0,0,0,0.35);
  border: 2px solid var(--line);
  border-top-color: #0b0a0a;
  border-left-color: #0b0a0a;
  border-radius: 4px;
  box-shadow: inset 0 2px 6px rgba(0,0,0,0.7);
}

/* ── Status / severity badges ─────────────────────────────────────────────
   The onboard-status badges (clean / pending) in project cards.
   ─────────────────────────────────────────────────────────────────────── */

.pg-onboard-badge {
  font-family: "Courier Prime", ui-monospace, monospace;
  font-weight: 700;
  box-shadow: 1px 1px 3px rgba(0,0,0,0.5);
}

/* ── Re-emit local button (issue #106) ────────────────────────────────────
   A dedicated button class for the "Re-emit rules locally" action: amber
   primary that reads as a sibling to .btn-run but at a slightly smaller
   size so it fits beside the PR-emit button without competing.
   ─────────────────────────────────────────────────────────────────────── */

.btn-emit-local {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  font: inherit;
  font-size: 13px;
  font-weight: 700;
  color: #fff;
  background: linear-gradient(180deg, var(--accent) 0%, var(--accent-ink) 100%);
  border: 2px solid var(--accent-ink);
  border-top-color: var(--accent);
  border-bottom-color: #7a5200;
  border-radius: 7px;
  padding: 9px 16px;
  cursor: pointer;
  text-shadow: 0 1px 2px rgba(0,0,0,0.5);
  box-shadow: 0 3px 8px rgba(0,0,0,0.45), inset 0 1px 0 rgba(255,255,255,0.12);
  transition: background .2s var(--ease), box-shadow .2s var(--ease), transform .15s var(--ease);
  position: relative;
  overflow: hidden;
}
.btn-emit-local::after {
  content: '';
  position: absolute;
  top: 1px; left: 1px; right: 1px;
  height: 35%;
  background: linear-gradient(to bottom, rgba(255,255,255,0.10) 0%, rgba(255,255,255,0) 100%);
  border-radius: 5px 5px 0 0;
  pointer-events: none;
}
.btn-emit-local:hover:not(:disabled) {
  background: linear-gradient(180deg, #dba012 0%, var(--accent-ink) 100%);
  box-shadow: 0 5px 14px rgba(0,0,0,0.55), inset 0 1px 0 rgba(255,255,255,0.14);
  transform: translateY(-1px);
}
.btn-emit-local:active:not(:disabled) {
  background: linear-gradient(180deg, #a16207 0%, #7a5200 100%);
  transform: translateY(1px);
  box-shadow: 0 1px 3px rgba(0,0,0,0.4), inset 0 2px 5px rgba(0,0,0,0.5);
}
.btn-emit-local:disabled {
  opacity: 0.45;
  cursor: not-allowed;
  transform: none;
  box-shadow: none;
}

/* ══════════════════════════════════════════════════════════════════════════════
   SURFACE 1 — 3-phase UoW nav-rail tabs
   Mockup: .phase-navigator-rail / .phase-nav-tab / .active / .finished /
           .phase-nav-name / .phase-status-indicator / .phase-nav-desc
   App:    .uow-phase-topbar (the row that holds status + tabs + stop)
           .uow-phase-tabs  (the flex container for the three tab buttons)
           .uow-phase-tab   (each button; + .active + .finished modifiers)
   ══════════════════════════════════════════════════════════════════════════════ */

/* Topbar row that holds the status pill, phase tabs, and optional stop button */
.uow-phase-topbar {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  padding: 12px 16px;
  border-bottom: 1.5px solid var(--line);
  background: rgba(26,24,22, var(--opacity-mid));
}

/* Status "Status: <value>" inline pair */
.uow-phase-status {
  display: flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
}
.uow-status-label {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: .05em;
  text-transform: uppercase;
  color: var(--ink-faint);
}
.uow-status-badge {
  font-size: 11.5px;
  font-weight: 600;
  color: var(--ink-soft);
  background: rgba(255,255,255,0.07);
  border: 1px solid rgba(255,255,255,0.12);
  border-radius: 999px;
  padding: 2px 9px;
}

/* The three-tab flex band */
.uow-phase-tabs {
  display: flex;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

/* Individual phase tab */
.uow-phase-tab {
  flex: 1;
  padding: 10px 14px;
  border-radius: var(--r-sm);
  border: 1.5px solid rgba(255,255,255,0.08);
  background: rgba(0,0,0,0.22);
  cursor: pointer;
  font-size: 13px;
  font-weight: 700;
  font-family: "Courier Prime", ui-monospace, monospace;
  color: var(--ink-soft);
  letter-spacing: 0.02em;
  text-align: left;
  position: relative;
  transition: all 0.25s var(--ease);
  box-shadow: 0 2px 6px rgba(0,0,0,0.18), inset 0 1px 0 rgba(255,255,255,0.04);
  /* Sheen */
  background-image: linear-gradient(to bottom, rgba(255,255,255,0.04) 0%, rgba(255,255,255,0) 50%);
}

.uow-phase-tab:hover {
  background: rgba(26,24,22,0.5);
  border-color: rgba(202,138,4,0.4);
  color: var(--ink);
  transform: translateY(-1px);
}

/* Active (currently selected) tab */
.uow-phase-tab.active {
  background: rgba(40,32,0,0.65);
  border-color: var(--accent);
  color: var(--ink);
  box-shadow: 0 6px 18px rgba(0,0,0,0.35), inset 0 1px 0 rgba(255,255,255,0.06);
}

/* Amber gradient underline on active tab — the key visual signal */
.uow-phase-tab.active::after {
  content: '';
  position: absolute;
  bottom: 0;
  left: 15%;
  width: 70%;
  height: 3px;
  background: linear-gradient(90deg, var(--warning-color), var(--accent));
  border-radius: 99px;
  box-shadow: 0 0 6px rgba(202,138,4,0.55);
}

/* Finished tab (phase done) */
.uow-phase-tab.finished {
  border-color: rgba(22,163,74,0.4);
  background: rgba(22,163,74,0.08);
  color: var(--ink-soft);
}

.uow-phase-tab.finished::before {
  content: '✓ ';
  color: var(--good);
  font-weight: 700;
}

/* Status indicator dot (sits before the phase name via inline rendering) */
.uow-phase-status-dot {
  display: inline-block;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: rgba(200,190,180,0.5);
  margin-right: 7px;
  vertical-align: middle;
  box-shadow: inset 0 1px 1px rgba(0,0,0,0.2);
}
.uow-phase-tab.active .uow-phase-status-dot {
  background: var(--accent);
  box-shadow: 0 0 5px rgba(202,138,4,0.7);
}
.uow-phase-tab.finished .uow-phase-status-dot {
  background: var(--good);
  box-shadow: 0 0 5px rgba(22,163,74,0.6);
}


/* ══════════════════════════════════════════════════════════════════════════════
   SURFACE 2 — Chat + clarification cards (Bletchley amber, legible foreground)
   Mockup: .dialog-card / .dialog-header / .dialog-opt-btn / .dialog-opt-btn.selected
   App:    .clarify-q-card / .clarify-q-question / .clarify-q-option / .on
           .uow-agent-chat-turn (+ .user / .ai role classes applied inline)
           .uow-agent-chat-text
           .chat-panel / .chat-head / .chat-log / .chat-turn / .chat-turn-text
   Readability adjustment: amber border-left accent, NOT amber backgrounds on text
   — foreground always uses --ink / --ink-soft so it stays legible on dark ground.
   ══════════════════════════════════════════════════════════════════════════════ */

/* Clarification question card: amber left accent + warm dark surface */
.clarify-q-card {
  border: 1.5px solid rgba(202,138,4,0.35);
  border-left: 4px solid var(--accent);
  background: rgba(26,24,22, var(--opacity-high));
  border-radius: var(--r-sm);
  box-shadow: 0 4px 14px rgba(0,0,0,0.35), inset 0 1px 0 rgba(255,255,255,0.04);
}

/* Question text inside the clarify card */
.clarify-q-question {
  color: var(--ink);
  font-weight: 700;
  font-size: 13.5px;
}

/* Addressee label below the question */
.clarify-q-addressee {
  color: var(--accent-ink);
}

/* Option buttons within the clarify card */
.clarify-q-option {
  border: 1.5px solid rgba(255,255,255,0.10);
  background: rgba(0,0,0,0.22);
  border-radius: var(--r-sm);
  padding: 9px 11px;
  transition: all 0.22s var(--ease);
  position: relative;
  overflow: hidden;
}
/* Subtle sheen */
.clarify-q-option::before {
  content: '';
  position: absolute;
  top: 0; left: 0; right: 0; height: 35%;
  background: linear-gradient(to bottom, rgba(255,255,255,0.05) 0%, rgba(255,255,255,0) 100%);
  pointer-events: none;
}
.clarify-q-option:hover {
  border-color: rgba(202,138,4,0.5);
  background: rgba(26,24,22,0.5);
  transform: translateX(2px);
  box-shadow: 0 3px 10px rgba(0,0,0,0.28);
}
/* Selected option: amber accent border + warm amber wash — text stays --ink for contrast */
.clarify-q-option.on {
  background: linear-gradient(135deg, rgba(60,40,0,0.65) 0%, rgba(30,20,0,0.55) 100%);
  border-color: var(--accent);
  box-shadow: 0 4px 12px rgba(0,0,0,0.35), inset 0 1px 0 rgba(255,255,255,0.05);
}
/* Option label text: keep full contrast */
.clarify-q-option-label {
  color: var(--ink);
  font-weight: 600;
}
.clarify-q-option-desc {
  color: var(--ink-soft);
}

/* Agent chat turns inside the UoW phase body */
.uow-agent-chat-turn {
  padding: 8px 11px;
  border-radius: 10px;
  background: rgba(26,24,22, var(--opacity-high));
  border: 1px solid var(--line);
}
/* "you" role: amber-washed bubble, text stays dark-on-amber-wash for contrast */
.uow-agent-chat-turn.user {
  align-self: flex-end;
  background: var(--accent-wash);
  border-color: rgba(202,138,4,0.3);
}
/* "ai" / engine role: standard dark surface, amber-left accent */
.uow-agent-chat-turn.ai {
  align-self: flex-start;
  border-left: 3px solid var(--accent);
  border-radius: 0 10px 10px 0;
}
/* Chat role label */
.uow-agent-chat-role {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: .06em;
  text-transform: uppercase;
  color: var(--accent-ink);
  display: block;
  margin-bottom: 4px;
}
/* Chat body text: always high-contrast --ink */
.uow-agent-chat-text {
  color: var(--ink);
  font-size: 13px;
  line-height: 1.5;
}

/* The research chat panel FAB window: amber border top + darker surface */
.chat-panel {
  border-color: var(--line);
  background: rgba(26,24,22, var(--opacity-high));
}

/* Chat header bar */
.chat-head {
  background: rgba(20,18,17,0.97);
  border-bottom: 1.5px solid var(--line);
}

/* Chat title in header */
.chat-title {
  color: var(--ink);
  font-family: "Courier Prime", ui-monospace, monospace;
  font-weight: 700;
}

/* Chat log area: same dark surface */
.chat-log {
  background: rgba(22,20,18, var(--opacity-low));
}

/* AI (engine) chat turn text: amber left border, dark bg, high-contrast --ink text */
.chat-turn.ai .chat-turn-text {
  background: rgba(26,24,22,0.92);
  border: 1px solid var(--line);
  border-left: 3px solid var(--accent);
  border-radius: 0 10px 10px 0;
  color: var(--ink);      /* legible warm-white on near-black */
}
/* User chat turn text: amber wash, muted amber border — keep text color at --ink */
.chat-turn.you .chat-turn-text {
  background: var(--accent-wash);
  border-color: rgba(202,138,4,0.28);
  color: var(--ink);      /* NOT amber text on amber wash — too low contrast */
}

/* Chat compose row */
.chat-compose {
  background: rgba(20,18,17,0.97);
  border-top: 1.5px solid var(--line);
}


/* ══════════════════════════════════════════════════════════════════════════════
   SURFACE 3 — Table chrome (.chorale-root container + scan/rules page wrappers)
   Mockup: .table-container — glass bg, 1.5px glass border, 6px radius, shadow
   App:    .chorale-root (chorale's own root wrapper on every table)
           .scan-results (the findings-table page section in the scan view)
           .routine-table (the routines table)
   We ONLY style the app-level container/chrome; chorale internals are left alone
   (they inherit the --chorale-* palette vars already set in :root).
   ══════════════════════════════════════════════════════════════════════════════ */

/* Chorale table outer chrome: glass card matching mockup .table-container */
.chorale-root {
  background: rgba(26,24,22, var(--opacity-high));
  border: 1.5px solid var(--line);
  border-radius: var(--r-sm);
  overflow: hidden;
  margin-top: 16px;
  box-shadow:
    0 15px 40px rgba(0,0,0,0.65),
    inset 0 1px 0 rgba(255,255,255,0.04),
    inset 0 -1px 3px rgba(0,0,0,0.35);
  position: relative;
}

/* ── Chorale Theme::Dark transparency override ──────────────────────────────
   Chorale injects its own <style> with .chorale-root[data-chorale-theme="dark"]
   AFTER our stylesheet, resetting --chorale-surface to opaque #1e1e1e and
   --chorale-header-bg to opaque #252526.  We fight back with !important so the
   bombe peeks through the table surface and header while cells stay readable.
   Inline styles reference var(--chorale-surface) / var(--chorale-header-bg), so
   overriding the vars here propagates to every cell/header automatically.
   Do NOT make popovers/inputs translucent (they have their own vars and must
   stay opaque for readability). ────────────────────────────────────────────── */
.chorale-root[data-chorale-theme="dark"] {
  --chorale-surface:    rgba(22,19,15,0.78) !important;   /* table body + frozen cells */
  --chorale-header-bg:  rgba(16,13,11,0.88) !important;   /* column headers + filter row */
  --chorale-toolbar-bg: rgba(16,13,11,0.88) !important;   /* pagination / export toolbar */
  --chorale-row-bg:     rgba(22,19,15,0.60) !important;   /* explicit row bg (light theme only, but set for parity) */
}

/* Amber accent line at the top of each table chrome, matching the topbar cable motif */
.chorale-root::before {
  content: '';
  position: absolute;
  top: 0; left: 0; right: 0; height: 2px;
  background: linear-gradient(90deg, transparent, var(--accent), transparent);
  opacity: 0.55;
  pointer-events: none;
  z-index: 1;
}

/* Scan results section wrapper: breathe around the table + own background */
.scan-results {
  background: transparent;
  border-radius: var(--r-sm);
}

/* Routines table chrome: same glass treatment */
.routine-table {
  background: rgba(26,24,22, var(--opacity-high));
  border: 1.5px solid var(--line);
  border-radius: var(--r-sm);
  overflow: hidden;
  box-shadow:
    0 10px 28px rgba(0,0,0,0.55),
    inset 0 1px 0 rgba(255,255,255,0.04);
}

/* Routine table header row: darker band matching mockup .chorale-thead */
.routine-head {
  background: rgba(20,18,17,0.92);
  border-bottom: 1.5px solid var(--line);
  color: var(--ink);
}

/* Routine table body rows: subtle warm ground, amber hover */
.routine-row:not(.routine-head) {
  background: transparent;
  transition: background 0.18s var(--ease);
}
.routine-row:not(.routine-head):hover {
  background: rgba(202,138,4,0.05);
}

/* ── Credentials panel ──────────────────────────────────────────────────────── */

.credentials-panel {
  padding: 24px 28px;
  max-width: 660px;
  display: flex;
  flex-direction: column;
  gap: 24px;
  overflow-y: auto;
  overflow-x: hidden;   /* overflow-y:auto implicitly makes x:auto; force-hide the h-scrollbar */
}
.credentials-title {
  margin: 0;
  font-size: 20px;
  font-weight: 600;
  color: var(--ink);
}
.credentials-intro {
  margin: 0;
  font-size: 13px;
  color: var(--ink-soft);
  line-height: 1.5;
}
.credentials-field-section {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 16px 20px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.credentials-field-header {
  display: flex;
  align-items: center;
  gap: 10px;
}
.credentials-label {
  font-size: 14px;
  font-weight: 600;
  color: var(--ink);
}
.credentials-badge-set {
  font-size: 11px;
  font-weight: 600;
  background: rgba(22,163,74,0.18);
  color: var(--good);
  border-radius: var(--r-sm);
  padding: 2px 7px;
  letter-spacing: 0.03em;
}
.credentials-badge-unset {
  font-size: 11px;
  font-weight: 500;
  background: var(--line-soft);
  color: var(--ink-faint);
  border-radius: var(--r-sm);
  padding: 2px 7px;
}
.credentials-hint {
  margin: 0;
  font-size: 12px;
  color: var(--ink-soft);
  line-height: 1.5;
}
.credentials-masked {
  margin: 0;
  font-size: 12px;
  font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  color: var(--ink-soft);
  background: var(--accent-wash);
  border-radius: var(--r-sm);
  padding: 3px 8px;
  display: inline-block;
}
.credentials-input-row {
  display: flex;
  gap: 10px;
  align-items: center;
}
.credentials-input {
  flex: 1;
  background: rgba(255,255,255,0.04);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  color: var(--ink);
  font-size: 13px;
  padding: 7px 10px;
  transition: border-color 0.15s var(--ease), box-shadow 0.15s var(--ease);
}
.credentials-input:focus {
  outline: none;
  border-color: var(--accent);
  box-shadow: 0 0 0 2px var(--accent-wash);
}
.credentials-save-btn {
  flex-shrink: 0;
}

/* ── Bombe animation settings (inside the Settings panel) ── */
.bombe-settings-section {
  margin-top: 28px;
  padding-top: 24px;
  border-top: 1px solid var(--line);
}
.bombe-settings-hint {
  font-size: 13px;
  color: var(--ink-faint);
  margin: 4px 0 14px;
  line-height: 1.5;
}
.bombe-settings-row {
  display: flex;
  gap: 20px;
  align-items: center;
  flex-wrap: wrap;
}
.bombe-settings-item {
  display: flex;
  align-items: center;
  gap: 10px;
}
.bombe-settings-item-label {
  font-size: 14px;
  color: var(--ink-soft);
  font-weight: 500;
  min-width: 52px;
}
/* ON/OFF toggle button */
.bombe-toggle-btn {
  font: inherit;
  font-size: 13px;
  font-weight: 700;
  letter-spacing: .05em;
  border-radius: 999px;
  padding: 6px 18px;
  border: 1.5px solid;
  cursor: pointer;
  transition: background 0.22s ease, color 0.22s ease, border-color 0.22s ease, box-shadow 0.22s ease;
}
.bombe-toggle-btn-on {
  background: var(--accent);
  color: #fff;
  border-color: var(--accent);
  box-shadow: 0 2px 8px rgba(202,138,4,0.30);
}
.bombe-toggle-btn-on:hover {
  background: var(--accent-ink);
  border-color: var(--accent-ink);
}
.bombe-toggle-btn-off {
  background: var(--paper);
  color: var(--ink-soft);
  border-color: var(--line);
}
.bombe-toggle-btn-off:hover {
  border-color: var(--accent);
  color: var(--accent-ink);
}
/* Play/Pause preview button */
.bombe-preview-btn {
  font: inherit;
  font-size: 13px;
  font-weight: 600;
  border-radius: 999px;
  padding: 6px 18px;
  border: 1.5px solid var(--line);
  background: var(--surface);
  color: var(--ink-soft);
  cursor: pointer;
  transition: background 0.22s ease, color 0.22s ease, border-color 0.22s ease;
}
.bombe-preview-btn:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent-ink);
}
.bombe-preview-btn-active {
  background: var(--accent-wash);
  border-color: var(--accent);
  color: var(--accent-ink);
}
.bombe-preview-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

"#;
