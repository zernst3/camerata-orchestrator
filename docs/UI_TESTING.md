# Testing the Dioxus UI (camerata-ui)

The UI is a Dioxus **desktop** crate (wry/WebKit, no wasm). Browser E2E (Playwright, which Dioxus
recommends but targets `dx serve` = web) is therefore NOT a fit and we deliberately skip it. What we
DO have, runnable in plain `cargo test -p camerata-ui` with no browser/wasm toolchain:

1. **Pure-logic unit tests** — the bulk of coverage. Extract logic out of components into plain
   functions and test those (e.g. `chat_model_groups`). Highest ROI; keep doing this first.
2. **Tier 1 — component render tests** (`dioxus-ssr`): render a component to an HTML string and assert
   its structure.
3. **Tier 2 — network-helper tests** (`wiremock`): point a `reqwest` helper at a fake BFF and assert
   the request it sends.

Both dev-deps live in `crates/ui/Cargo.toml`.

---

## Tier 1: render tests (dioxus-ssr)

Render a component headlessly and assert the HTML contains the expected text/elements. Catches the
"component renders the wrong shape / an element vanished" class of bug (e.g. the recurring
model-selector-disappeared regression).

**The pattern** (see `crates/ui/src/cockpit/live_run.rs` → `mod render_tests`):

```rust
#[cfg(test)]
mod render_tests {
    use super::{ClarificationView, ClarifyQuestion};
    use dioxus::prelude::*;

    // A root component that mounts the component under test. It runs INSIDE the VirtualDom runtime,
    // so the inner component's hooks + event-handler creation work. (render_element() alone panics
    // with "Must be called from inside a Dioxus runtime" for a component that uses hooks.)
    fn harness() -> Element {
        rsx! {
            ClarifyQuestion {
                clar: ClarificationView { question: "Which storage backend?".into(), ..Default::default() },
                on_answered: move |_| {},
            }
        }
    }

    #[test]
    fn renders_question_and_submit() {
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Which storage backend?"));
        assert!(html.to_lowercase().contains("submit"));
    }
}
```

**Caveats — what render tests CAN'T do:**

- **No interaction.** SSR renders static HTML; you cannot simulate a click or assert state changes
  from events. Assert presence/structure, not behavior. (Test the *behavior* of an `onclick` by
  extracting its logic into a function, or by testing the network helper it calls — Tier 2.)
- **Components that need context** (`use_context`, e.g. the toast signal) must have that context
  PROVIDED in the harness — wrap the component in a root that calls `use_context_provider(...)` first,
  or they panic. Prop-only / `use_signal`-only components (like `ClarifyQuestion`) need no setup.
- **`dangerous_inner_html` does not render under SSR** ([Dioxus #941](https://github.com/DioxusLabs/dioxus/issues/941)).
  The chat AI bubble uses it for markdown, so a render test there can assert the surrounding structure
  (the bubble, the "+ Add to learnings" button) but NOT the rendered markdown body.
- **`use_resource` (async fetches) are pending** on first render — the component renders its
  loading/fallback branch, not the loaded data.

---

## Tier 2: network-helper tests (wiremock)

The cockpit's `reqwest` helpers talk to the BFF. To test one, point it at a `wiremock` mock server via
the `CAMERATA_BFF_URL` seam (`crate::bff_base()` in `main.rs`) and assert the request it issues.

**Make the helper testable:** call `crate::bff_base()` instead of `crate::BFF_URL` directly (it
returns the `CAMERATA_BFF_URL` env override, else the production const). Convert helpers as you add
tests for them.

**The pattern** (see `crates/ui/src/chat.rs` → `add_chat_learning_resolves_active_then_posts_the_reply`):

```rust
#[tokio::test]
async fn helper_sends_the_right_request() {
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": "proj-7" })))
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/api/projects/proj-7/memory"))
        .and(body_json(serde_json::json!({ "kind": "decision", "text": "X" })))  // asserts the body
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .expect(1)  // verified on server drop
        .mount(&server).await;

    std::env::set_var("CAMERATA_BFF_URL", server.uri());
    let ok = super::add_chat_learning("X").await;
    std::env::remove_var("CAMERATA_BFF_URL");
    assert!(ok);
}
```

**Caveat:** `CAMERATA_BFF_URL` is a process-global env var, so a mock-server test that sets it must not
run concurrently with another test that reads `bff_base()`. Keep these tests narrowly scoped (today
only `add_chat_learning` reads `bff_base()`); if more helpers are converted and tested, gate the
env-setting tests behind a shared `serial_test`-style mutex.

---

## What to test, in ROI order

1. Extract + unit-test pure logic (the default; most bugs live here).
2. Tier-1 render tests for the components most prone to "shape" regressions (the chat panel's model
   selector, the settings editors, the review panel).
3. Tier-2 wiremock tests for the network helpers whose request contract matters.

Skip Tier-3 (browser E2E) on the desktop crate until/unless we add a `dioxus-web` target.
