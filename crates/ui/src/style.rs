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
  --accent:     #c8694a;   /* terracotta — the single accent */
  --accent-ink: #a8523a;   /* accent text/hover */
  --accent-wash:#f7ece7;   /* accent at 8% for fills */
  --good:       #5c8a5c;   /* the quiet check */

  --r-lg: 22px;
  --r-md: 16px;
  --r-sm: 11px;

  --shadow-card: 0 1px 2px rgba(27,26,24,.04), 0 10px 30px rgba(27,26,24,.05);
  --shadow-pop:  0 1px 2px rgba(27,26,24,.05), 0 18px 50px rgba(27,26,24,.09);

  /* Slow, reassuring easing. Nothing snaps. */
  --ease: cubic-bezier(.22,.61,.36,1);
}

* { box-sizing: border-box; }

html, body {
  margin: 0;
  padding: 0;
  height: 100%;
  background: var(--paper);
  color: var(--ink);
  font-family: system-ui, BlinkMacSystemFont, "Segoe UI",
               Roboto, Helvetica, Arial, sans-serif;
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
}

.app-root {
  min-height: 100vh;
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
"#;
