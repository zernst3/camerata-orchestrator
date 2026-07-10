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
// console,extra}, severity, status, ts). The capture endpoint itself
// (`/api/feedback` by default) is NOT implemented by this skeleton — that's Part 2
// of the scaffolder. Until it exists these POSTs 404 harmlessly.
(function () {
  var CAPTURE_URL = window.CAMERATA_CAPTURE_URL || "/api/feedback";
  var PROJECT_ID = window.CAMERATA_PROJECT_ID || "{{APP_NAME_SNAKE}}";

  function nowIso() {
    return new Date().toISOString();
  }

  function post(report) {
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
    return {
      id: null,
      project_id: PROJECT_ID,
      source: "auto",
      kind: "runtime_error",
      title: title,
      description: description || "",
      context: {
        route: window.location ? window.location.pathname : null,
        element: null,
        stack: stack || null,
        console: null,
        extra: {},
      },
      severity: "error",
      status: "open",
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
