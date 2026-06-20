//! The global stylesheet. Kept as one string so the whole look — palette, type
//! scale, spacing, motion — lives in a single, reviewable place. The look is
//! restrained consumer-grade: near-black on near-white, one warm accent
//! (terracotta, nodding to the pottery example), a system-font stack, large calm
//! type, slow subtle motion.

pub const GLOBAL_CSS: &str = r#"
:root {
  /* Restrained palette: near-black text, near-white ground, one accent. */
  --ink:        #1b1a18;   /* near-black, warm */
  --ink-soft:   #6c6862;   /* secondary text */
  --ink-faint:  #a8a39b;   /* tertiary / hints */
  --paper:      #faf9f6;   /* near-white, warm */
  --surface:    #ffffff;   /* raised cards */
  --line:       #ece9e3;   /* hairline borders */
  --line-soft:  #f3f1ec;
  --accent:     #b35636;   /* terracotta — deepened ~12% so it reads "considered" */
  --accent-ink: #97442a;   /* accent text/hover (deeper still) */
  --accent-wash:#f5e9e3;   /* accent at ~8% for fills */
  --good:       #5c8a5c;   /* the quiet check */

  /* Enterprise-sharp corners (Linear/Vercel/Stripe register), not consumer-round. */
  --r-lg: 12px;
  --r-md: 8px;
  --r-sm: 5px;

  --shadow-card: 0 1px 2px rgba(27,26,24,.04), 0 10px 30px rgba(27,26,24,.05);
  --shadow-pop:  0 1px 2px rgba(27,26,24,.05), 0 18px 50px rgba(27,26,24,.09);

  /* Slow, reassuring easing. Nothing snaps. */
  --ease: cubic-bezier(.22,.61,.36,1);

  /* chorale table palette → mapped onto the app's warm cream/terracotta scheme so the
     grouped tables stop reading as the library's default blue. (chorale exposes these as
     overridable CSS variables.) */
  --chorale-accent:            var(--accent);
  --chorale-accent-contrast:   #ffffff;
  --chorale-surface:           var(--surface);
  --chorale-text:              var(--ink);
  --chorale-text-muted:        var(--ink-soft);
  --chorale-text-subtle:       var(--ink-faint);
  --chorale-text-disabled:     var(--ink-faint);
  --chorale-border:            var(--line);
  --chorale-divider:           var(--line);
  --chorale-separator-color:   var(--line);
  --chorale-header-bg:         var(--paper);
  --chorale-group-header-bg:   var(--accent-wash);
  --chorale-group-header-border: var(--line);
  --chorale-toolbar-bg:        var(--surface);
  --chorale-input-bg:          var(--surface);
  --chorale-input-border:      var(--line);
  --chorale-button-bg:         var(--surface);
  --chorale-button-disabled-bg: var(--line-soft);
  --chorale-popover-bg:        var(--surface);
  --chorale-range-bg:          var(--accent-wash);
  --chorale-active-cell-outline: var(--accent);
  --chorale-row-selected-divider: var(--accent);
  --chorale-error:             #b3261e;
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
  background: var(--paper);
  color: var(--ink);
  font-family: "Inter", ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont,
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
.page-wide { max-width: 860px; }

@keyframes rise {
  from { opacity: 0; transform: translateY(14px); }
  to   { opacity: 1; transform: translateY(0); }
}
@keyframes fade {
  from { opacity: 0; }
  to   { opacity: 1; }
}
@keyframes slideIn {
  from { opacity: 0; transform: translateY(10px); }
  to   { opacity: 1; transform: translateY(0); }
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
@keyframes pop {
  0% { transform: scale(.4); opacity: 0; }
  60% { transform: scale(1.12); }
  100% { transform: scale(1); opacity: 1; }
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
  background: #fff; border: 1px solid #efd9d0;
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
.qa-check.on { background: #f4f8f4; border-color: #cfe2cf; }
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
  background: #f0d4cb;
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
  background: #fdf4e6;                 /* soft amber wash, warmer than an alert red */
  border-bottom: 1px solid #f0e2c8;
  color: #7a5a2e;
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
.cockpit { display: flex; flex-direction: column; flex: 1; min-height: 0; width: 100%; background: var(--paper); }

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
.toast.info { border-color: #b9c6d6; }
.toast.info .toast-label { background: #e7eef6; color: #355b86; }
.toast.warning { border-color: #f0c89a; background: #fff8ef; }
.toast.warning .toast-label { background: #f6e2c4; color: #8a4f1d; }
.toast.error { border-color: #e6a8a0; background: #fdf2f0; }
.toast.error .toast-label { background: #f4cfc8; color: #9a3526; }

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
.onboard-gate { display: flex; gap: 10px; align-items: flex-start; background: #fff7ed; border: 1px solid #f0c89a; border-radius: 10px; padding: 12px 14px; margin-bottom: 16px; }
.onboard-gate-dot { width: 8px; height: 8px; border-radius: 50%; background: #b06a2e; margin-top: 5px; flex: none; }
.onboard-gate-h { font-weight: 700; font-size: 13px; color: #8a4f1d; }
.onboard-gate-b { font-size: 12px; color: #8a4f1d; margin-top: 3px; line-height: 1.5; }
.mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .92em; }
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
.onboard-repo-chip-x:hover { background: #f3e6e0; color: var(--ink); }

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
.scan-note { font-size: 12px; color: #8a4f1d; background: #fff8ef; border: 1px solid #f0c89a; border-radius: 8px; padding: 8px 11px; margin-bottom: 12px; }
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
.alt-row.must { background: #fff8ef; border-radius: 8px; padding: 10px 12px; border-bottom: none; margin: 4px 0; }
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
.rule-count { display: flex; flex-direction: column; min-width: 150px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
.rule-count-n { font-size: 22px; font-weight: 700; color: var(--ink); }
.rule-count-l { font-size: 12px; color: var(--ink-soft); }
.applied-list { display: flex; flex-direction: column; gap: 10px; margin-top: 12px; }
.applied-rule { border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; background: var(--surface); }
.applied-rule-head { display: flex; align-items: center; gap: 10px; }
.applied-rule-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; font-weight: 700; color: var(--ink); }
.applied-rule-repo { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; color: var(--ink-faint); }
.applied-tag { font-size: 10px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; padding: 2px 6px; border-radius: 5px; }
.applied-tag.custom { background: #e7eef6; color: #355b86; }
.applied-tag.drift { background: #fdf2f0; color: #9a3526; }
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
.cockpit-rail { border-right: 1px solid var(--line); background: #fbfaf7; }
.cockpit-inspector { border-left: 1px solid var(--line); background: #fbfaf7; }
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
.spine-badge.neutral, .topbar-status.neutral { background: #ece9e3; color: #6c6862; }
.spine-badge.active,  .topbar-status.active  { background: #e7eef7; color: #2f5f9e; }
.spine-badge.warn,    .topbar-status.warn    { background: #fbecd6; color: #9a6418; }
.spine-badge.done,    .topbar-status.done    { background: #e2f1e7; color: #2f8f5b; }
.spine-badge.block,   .topbar-status.block   { background: #f7e1dc; color: #b0432e; }

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
/* Segmented control for the 3-state dev-status selector. */
.uow-seg {
  display: inline-flex; border: 1px solid var(--line); border-radius: 7px; overflow: hidden;
  background: var(--paper);
}
.uow-seg-btn {
  font-size: 11.5px; font-weight: 600; padding: 4px 11px;
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
  border: 1px solid #f0e2c8; background: #fdf8ef; border-radius: 9px;
  padding: 9px 10px; cursor: pointer; font-size: 12.5px; color: var(--ink); line-height: 1.35;
}
.needs-item:hover { border-color: var(--accent); }
.needs-dot { flex: none; width: 8px; height: 8px; border-radius: 50%; margin-top: 4px; }
.needs-dot.warn { background: #d9a441; }
.needs-q { display: block; }
.needs-who { display: block; margin-top: 2px; font-size: 11px; color: var(--ink-faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.needs-empty { font-size: 12.5px; color: var(--ink-faint); font-style: italic; margin: 0; }

/* Center stage */
.cockpit-stage { display: flex; flex-direction: column; min-width: 0; padding: 14px 18px; }
.stage-tabs { display: flex; gap: 6px; margin-bottom: 14px; }
.stage-tab {
  font-size: 10.5px; font-weight: 700; letter-spacing: .05em; color: var(--ink-faint);
  padding: 4px 10px; border-radius: 6px; background: #f1efe9;
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
.gate-layer.l1 { background: #f7e1dc; color: #b0432e; }   /* deny-before-execute */
.gate-layer.l2 { background: #fbecd6; color: #9a6418; }   /* post-task bounce */
.gate-event-text { font-size: 12.5px; color: var(--ink-soft); line-height: 1.45; margin: 0; }
.gate-rule { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; font-weight: 700; color: var(--ink); }

/* Done / provenance panel */
.prov-line { display: flex; gap: 12px; padding: 8px 0; border-bottom: 1px solid var(--line-soft); font-size: 13px; }
.prov-k { flex: none; width: 100px; color: var(--ink-faint); font-weight: 600; }
.prov-v { color: var(--ink); }

/* Blocked panel */
.blocked-reason { font-size: 13px; color: var(--ink-soft); line-height: 1.5; max-width: 60ch; background: #f7e1dc55; border: 1px solid #f0d2c8; border-radius: 9px; padding: 12px 14px; }

/* Status strip (always visible under the stage) */
.status-strip { margin-top: 12px; border-top: 1px solid var(--line); padding-top: 12px; display: flex; align-items: center; gap: 16px; flex-wrap: wrap; }
.strip-fleet { display: flex; align-items: center; gap: 8px; }
.fleet-pill { display: inline-flex; align-items: center; gap: 7px; border: 1px solid var(--line); border-radius: 20px; padding: 4px 11px; background: var(--surface); }
.fleet-pill.gated { border-color: #b6dcc4; background: #f0f8f3; }
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
  padding: 40px; background: var(--paper);
}

/* Cockpit internal nav: control surface vs routines (both architect tools). */
.cockpit-nav { display: flex; gap: 4px; padding: 7px 16px; background: #f1efe9; border-bottom: 1px solid var(--line); }
.cockpit-nav-tab {
  border: none; background: transparent; color: var(--ink-soft);
  font-size: 12.5px; font-weight: 700; padding: 5px 13px; border-radius: 7px; cursor: pointer;
}
.cockpit-nav-tab:hover { color: var(--ink); }
.cockpit-nav-tab.on { background: var(--surface); color: var(--ink); box-shadow: var(--shadow-card); }
.cockpit-scroll { flex: 1; overflow-y: auto; overflow-x: hidden; min-width: 0; }
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
.btn-secondary:hover { border-color: var(--accent); color: var(--accent-ink); }
.btn-secondary.danger:hover { border-color: #c0392b; color: #c0392b; }
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
.live-run { border: 1px solid var(--line); border-radius: 11px; background: var(--surface); padding: 14px 16px; }
.live-run-head { display: flex; align-items: center; gap: 12px; margin-bottom: 4px; }
.live-run-title { font-size: 15px; font-weight: 700; color: var(--ink); }
.live-run-status { font-size: 11px; font-weight: 700; letter-spacing: .04em; padding: 3px 9px; border-radius: 6px; }
.live-events { display: flex; flex-direction: column; gap: 9px; margin-top: 12px; }
.live-event { border-left: 3px solid var(--ink-faint); border-radius: 0 8px 8px 0; background: #fbfaf7; padding: 9px 12px; }
.live-event.deny { border-left-color: #b0432e; background: #f7e1dc55; }
.live-event.allow { border-left-color: #2f8f5b; background: #f0f8f3; }
.live-event.info { border-left-color: var(--ink-faint); background: #fbfaf7; }
.live-event.info .live-event-verdict { color: var(--ink-soft); }
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
.clar-answered { background: #f0f8f3; border-radius: 8px; padding: 8px 10px; }
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
.routine-table { margin-top: 18px; border: 1px solid var(--line); border-radius: 11px; overflow: hidden; background: var(--surface); }
.routine-row {
  display: grid; grid-template-columns: 2.4fr 1fr 1.4fr 1.4fr auto; gap: 14px;
  align-items: center; padding: 12px 16px; border-bottom: 1px solid var(--line-soft);
}
.routine-row:last-child { border-bottom: none; }
.routine-head { background: #fbfaf7; font-size: 11px; font-weight: 700; letter-spacing: .05em; color: var(--ink-faint); }
.routine-row.off { opacity: .55; }
.routine-name { display: flex; flex-direction: column; gap: 3px; }
.routine-title { font-size: 13.5px; font-weight: 600; color: var(--ink); }
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
.sched-picker { margin: 4px 0 12px; padding: 12px; border: 1px solid var(--line); border-radius: 10px; background: #fbfaf7; }
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
  background: #1b1a18; border: 1px solid #2e2d2a; border-radius: var(--r-md);
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
.agent-drawer { margin-top: 10px; border: 1px solid var(--line); border-radius: 10px; background: #fbfaf7; overflow: hidden; }
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
.fix-panel { margin: 18px 0; padding: 14px 16px; border: 1px solid var(--line); border-radius: 12px; background: #fbfaf7; }
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

/* Estimated token-usage badge in the Agent-activity detail. */
.agent-tokens {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 10.5px;
  font-weight: 700; color: var(--accent-ink); background: var(--accent-wash);
  border-radius: 999px; padding: 1px 8px; margin-left: 6px; letter-spacing: .02em;
}
.rule-modal-overlay { position: fixed; inset: 0; z-index: 1100; background: rgba(27,26,24,.34); display: flex; align-items: center; justify-content: center; padding: 24px; }
.rule-modal { width: 100%; max-width: 640px; max-height: 84vh; overflow-y: auto; background: var(--surface); border-radius: var(--r-md); box-shadow: var(--shadow-pop); padding: 22px 24px; }
.rule-modal-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.rule-modal-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; font-weight: 700; color: var(--accent-ink); }
.rule-modal-close { border: none; background: transparent; font-size: 16px; color: var(--ink-soft); cursor: pointer; padding: 2px 6px; }
.rule-modal-close:hover { color: var(--ink); }
.rule-modal-title { font-size: 17px; font-weight: 700; color: var(--ink); margin: 8px 0 12px; line-height: 1.35; }
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
  margin-top: 6px; font-size: 12.5px; color: #2f8f5b; font-weight: 600;
  background: #f0f8f3; border: 1px solid #c5e3ce; border-radius: var(--r-sm);
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
"#;
