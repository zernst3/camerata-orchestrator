// Auto-capture error reporter (Camerata feedback loop — see
// docs/plans/2026-07-09_product-owner-head-vibe-mode.md, "Feedback loop: auto-capture
// + click-to-report"). Runs entirely client-side: catches runtime errors the app
// itself hits and POSTs a `DefectReport`-shaped JSON payload (source="auto") to the
// capture endpoint, so defects surface to the governed dev loop before a human
// has to notice and report them.
//
// This file is loaded in <head>, before the wasm bundle (see index.html), so it is
// listening from the very first tick. It never throws and never blocks the app it
// is reporting on — every reporting call is best-effort (swallowed on failure).
//
// Matches the wire shape of `DefectReport` in `crates/api-types/src/feedback.rs`
// (id, project_id, source, kind, title, description, context{route,element,stack,
// console,extra}, severity, status, fingerprint, count, ts). `fingerprint`/`count`
// back the dedupe fold (see the fingerprint + rate-limit section below): this
// reporter computes its own fingerprint and rate-limits per fingerprint so a
// render-loop error storm never leaves the browser, and the ingest server folds
// repeats of the same fingerprint into one row by incrementing `count`.
(function () {
  var CAPTURE_URL = window.CAMERATA_CAPTURE_URL || "/api/feedback";
  var PROJECT_ID = window.CAMERATA_PROJECT_ID || "{{APP_NAME_SNAKE}}";

  function nowIso() {
    return new Date().toISOString();
  }

  // ---- Fingerprint + per-fingerprint rate limit (the dedupe fold, client side) ----
  // Mirrors the ingest server's dedupe key (kind + top stack frame + route) — see
  // `crates/server/src/lib.rs`'s `compute_feedback_fingerprint` and
  // `crates/api-types/src/feedback.rs`'s `DefectReport.fingerprint`/`count` fields —
  // so a render-loop error storm gets caught HERE, client-side, before it ever
  // leaves the browser: the cheapest possible fold. Dependency-free: a small FNV-1a
  // string hash, not the server's cryptographic SHA-256 (`content_hash`) — the two
  // algorithms do not need to byte-match. What matters is that THIS client computes
  // the SAME fingerprint for repeat occurrences of the SAME logical error, so (a) this
  // reporter's own rate limit can recognize a repeat, and (b) the server's dedupe
  // (which accepts whatever fingerprint the client sends, see `submit_feedback`) can
  // fold repeats across separate POSTs too.
  function fingerprintOf(kind, stack, route) {
    var topFrame = (stack || "").split("\n")[0].trim();
    var raw = kind + "|" + topFrame + "|" + (route || "");
    var hash = 0x811c9dc5; // FNV-1a 32-bit offset basis
    for (var i = 0; i < raw.length; i++) {
      hash ^= raw.charCodeAt(i);
      hash = (hash * 0x01000193) >>> 0; // FNV-1a prime; keep an unsigned 32-bit result
    }
    return hash.toString(16);
  }

  var RATE_LIMIT_WINDOW_MS = 60000; // 1 minute
  var RATE_LIMIT_MAX_PER_FINGERPRINT = 5; // cap occurrences of the SAME error per window
  var seenFingerprints = {}; // fingerprint -> { count, windowStart }

  // True when `fingerprint` has NOT yet hit its cap for the current window. A
  // render-loop / retry storm repeating the SAME error is exactly what this guards
  // against — after the cap, further identical-fingerprint occurrences within the
  // window are dropped locally (never POSTed at all), so the storm never leaves the
  // app, let alone the browser.
  function allowedByRateLimit(fingerprint) {
    var now = Date.now();
    var entry = seenFingerprints[fingerprint];
    if (!entry || now - entry.windowStart > RATE_LIMIT_WINDOW_MS) {
      seenFingerprints[fingerprint] = { count: 1, windowStart: now };
      return true;
    }
    entry.count += 1;
    return entry.count <= RATE_LIMIT_MAX_PER_FINGERPRINT;
  }

  function post(report) {
    if (report.fingerprint && !allowedByRateLimit(report.fingerprint)) {
      // Rate-limited: drop silently. The server-side dedupe fold (count increment)
      // handles the ones that DO get through; this is the earlier, cheaper fold that
      // keeps a storm of the SAME error from ever leaving the browser.
      return;
    }
    try {
      fetch(CAPTURE_URL, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(report),
        keepalive: true,
      }).catch(function () {
        // Best-effort: a capture-endpoint failure (e.g. 404 before Part 2 ships)
        // must never surface to the user or recurse into another report.
      });
    } catch (e) {
      // Swallow synchronous failures too (e.g. JSON.stringify on a cyclic object).
    }
  }

  function buildReport(title, description, stack) {
    var route = window.location ? window.location.pathname : null;
    var kind = "runtime_error";
    return {
      id: null,
      project_id: PROJECT_ID,
      source: "auto",
      kind: kind,
      title: title,
      description: description || "",
      context: {
        route: route,
        element: null,
        stack: stack || null,
        console: null,
        extra: {},
      },
      severity: "error",
      status: "open",
      fingerprint: fingerprintOf(kind, stack || "", route || ""),
      count: 1,
      ts: nowIso(),
    };
  }

  // 1. Uncaught synchronous errors (including JS errors and, via the wasm runtime,
  //    unrecovered Rust panics that propagate as a JS exception).
  window.addEventListener("error", function (event) {
    var stack = event.error && event.error.stack;
    post(buildReport(event.message || "Uncaught error", stack || "", stack));
  });

  // 2. Uncaught promise rejections.
  window.addEventListener("unhandledrejection", function (event) {
    var reason = event.reason;
    var message = (reason && reason.message) || String(reason);
    var stack = reason && reason.stack;
    post(buildReport("Unhandled promise rejection: " + message, stack || "", stack));
  });

  // 3. Failed network requests — a non-OK response or a network-level failure from
  //    this app's own `fetch` calls (server functions use `fetch` under the hood).
  var originalFetch = window.fetch;
  if (originalFetch) {
    window.fetch = function () {
      var args = arguments;
      var requestLabel = args[0] && args[0].toString ? args[0].toString() : String(args[0]);
      return originalFetch.apply(this, args).then(
        function (response) {
          if (!response.ok) {
            post(buildReport("Failed request: " + requestLabel + " (" + response.status + ")", "", ""));
          }
          return response;
        },
        function (err) {
          post(buildReport("Network error: " + requestLabel, (err && err.message) || String(err), err && err.stack));
          throw err;
        }
      );
    };
  }

  // 4. Bridge for the Rust wasm panic hook (see src/wasm_bridge.rs), which calls
  //    this directly since a panic hook has no JS `Error` object to hand `onerror`.
  window.__camerataReportPanic = function (message, stack) {
    post(buildReport("Wasm panic: " + message, stack || "", stack));
  };
})();
