# Phase 2b-5 — Webview surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a JavaScript guest read, write and watch the shared store over real `wry` IPC — and prove it by having JS write a value that Rust then reads back.

**Architecture:** A `WebviewSurface` owns a `wry` `WebView` in a `tao` window. Its IPC handler decodes a `duet_protocol::Request`, calls `dispatch` with the **host-supplied** `SubscriberId`, and posts the `Response` back with `evaluate_script`. Pushed notifications take the same return path. The pieces that can be tested without a display — request routing, response framing, push framing — sit behind a small seam; only the `wry` wiring itself needs a window.

**Tech Stack:** Rust 1.92, edition 2024. `wry` 0.56 and `tao` 0.36 (already `duet-backend-macos` dependencies), plus `duet-protocol`.

**Reference:** spec §6.3 — *"Webview — Tauri v2 IPC: `invoke` for commands, Tauri events for patches."*
**Evidence:** Spike B ran a `wry` webview alongside a Flutter engine for 180 s, with 696 `evaluate_script` round trips landing and JS-driven DOM content captured in a rasterized PNG. The mechanism is proven; this wires it to the real protocol.

---

## Background for the implementer

### What already exists

`duet-protocol` (merged) defines the conversation and serves it:

```rust
duet_protocol::decode_request(&serde_json::Value) -> Result<Request, CodecError>
duet_protocol::dispatch(&StoreHandle, SubscriberId, Request) -> Response   // never fails
duet_protocol::encode_response(&Response) -> serde_json::Value
duet_protocol::encode_push(&Push) -> serde_json::Value
```

`dispatch` is **total** — every error becomes a `Response::Failed` — so a guest that sends a well-formed request always gets a well-formed answer. That is what lets a transport treat it as infallible.

`duet-backend-macos` (merged) already has `ProxySink`, `FlutterEngine` and `MacBackend`, and links against real `tao`/`wry`. Spike B's webview code is at `spikes/spike-b-macos/src/main.rs`.

### The security property you must not break

`Request::Subscribe` carries **no** `SubscriberId`. The host supplies it. Each surface owns exactly one, allocated by `duet-runtime`.

If the IPC handler let a guest influence which subscriber it dispatches as, the webview could subscribe **as the Flutter surface** and receive its notifications — a confidentiality breach between two separate guest processes. The handler must capture its own surface's `SubscriberId` at construction and never read one off the wire.

### Two `wry` details Spike B established the hard way

1. **`evaluate_script_with_callback` double-encodes returned strings.** Returning an already-stringified JSON string from evaluated JS produces a doubly-escaped result, because `wry` runs the return value through `NSJSONSerialization`. **Return a plain JS object and let `wry` serialize it once.** This cost real debugging time in Spike B.

2. **`deliver` must not block or serialize.** The `Sink` runs on the core thread; anything done there is head-of-line latency for every subsequent reader. Post the batch to the UI thread and return — `ProxySink` already does exactly this.

### What can and cannot be verified here

Spike A established this machine has **no reachable on-screen WindowServer for spawned processes**. Windows render but nothing appears on a display.

**Verifiable:** JS executes and its DOM changes (Spike B captured this in a PNG); an IPC round trip completes; **a value written by JS is readable from Rust** — which is the shared-state claim for the webview side, and the deliverable here.

**Not verifiable, and must not be claimed:** real mouse or keyboard input. Spike B found synthetic events reach a Flutter view but **not** a `WKWebView`, unexplained. Do not assert anything about user interaction.

---

## Standing quality bar

Every item was a real review finding earlier in this project that cost a round trip.

- Every public item documented, including every variant and field; `#![deny(missing_docs)]`.
- `# Errors` sections on every `Result` return.
- No tautological assertions; **pin exact counts, not loose bounds.**
- **Close the loop the real system closes.** Seven times in this project a correct test has been paired with input that could not fail it. Where a JS round trip is involved, assert on the **value that came back**, not merely that a call was made.
- Verify each test genuinely fails before the implementation exists.
- Functions under 50 lines; no `unwrap`/`expect` in non-test, non-example code.
- Every `unsafe` block carries a `// SAFETY:` comment. `duet-backend-macos` does not `forbid(unsafe_code)` because it calls Objective-C.
- This crate stays **excluded from the coverage gate** — CI has no window server. Do not lower the threshold for the others.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-webview/src/lib.rs` | **Platform-free.** IPC routing: `handle_ipc_text`, `decode`, `response_script`, `push_script` |
| `crates/duet-webview/src/bootstrap.rs` | **Platform-free.** The bootstrap HTML and JS client |
| `crates/duet-backend-macos/src/webview.rs` | macOS-only: `WebviewSurface`, the `wry` wiring |
| `crates/duet-backend-macos/src/sink.rs` | Add `DuetEvent::WebviewScript` |
| `crates/duet-backend-macos/examples/webview_state.rs` | The proof: JS writes, Rust reads back |

**Amended after Task 1.** The routing logic and the bootstrap have no platform dependencies, and the emitted JavaScript is identical on macOS, Windows and Linux. They originally lived in `duet-backend-macos`, which CI cannot build — `.github/workflows/duet.yml` runs `ubuntu-latest` and passes `--exclude duet-backend-macos` to *every* step, so those tests never ran upstream. Phase 5's Windows and Linux backends would also have needed the same code. Both now live in a new platform-free `duet-webview` crate that CI covers automatically. `duet-backend-macos` keeps only what genuinely touches the platform.

---

## Task 1: The IPC handler, without `wry`

The routing is testable with no window; only the `wry` wiring is not. Separate them.

**Files:**
- Create: `crates/duet-backend-macos/src/webview.rs`
- Modify: `crates/duet-backend-macos/Cargo.toml`, `crates/duet-backend-macos/src/lib.rs`

**Baseline before this task:** `cargo test -p duet-backend-macos` reports **2 passed, 1 ignored** (the ignored one needs the main thread for `tao`'s event loop).

- [ ] **Step 1: Add the two missing dependencies**

`duet-backend-macos` currently depends on `duet-core`, `duet-runtime`, `duet-supervisor`, `duet-host`, `tao`, `wry` and the `objc2` crates. It has neither `duet-protocol` nor `serde_json`. Add both to `[dependencies]` in `crates/duet-backend-macos/Cargo.toml`:

```toml
duet-protocol = { path = "../duet-protocol" }
serde_json = { version = "1", features = ["float_roundtrip"] }
```

**`float_roundtrip` is mandatory, not stylistic.** Phase 2b-0 measured serde_json's default parser corrupting **296,628 of 1,000,000** finite `f64` values through a text hop. This crate's `decode` calls `serde_json::from_str` on guest text — the same parsing path.

Declare it here even though it is currently redundant. `cargo tree -p duet-backend-macos -e features -i serde_json` shows `float_roundtrip` already enabled by `duet-codec` and `duet-protocol`, and Cargo unifies features across the graph — so **no build, workspace or `-p`, would expose its absence here.** That is precisely why it must be written down: the protection is currently an accident of someone else's manifest, and would vanish silently if `duet-protocol` ever stopped depending on `serde_json`. A direct dependency states its own requirements.

- [ ] **Step 2: Write the failing test**

Create `crates/duet-backend-macos/src/webview.rs`:

```rust
//! The webview surface: a `wry` WebView speaking `duet-protocol`.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriberId, Value};
    use duet_runtime::{NullSink, Runtime};

    fn rt() -> Runtime {
        Runtime::spawn(
            Value::map([("editor", Value::map([("zoom", Value::Float(1.0))]))]),
            NullSink,
        )
    }

    #[test]
    fn a_get_request_is_answered_with_the_stored_value() {
        let rt = rt();
        let reply = handle_ipc_text(
            &rt.handle(),
            SubscriberId(1),
            r#"{"kind":"get","id":"1","path":"editor.zoom"}"#,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("the reply must be valid JSON");
        assert_eq!(parsed["kind"], "value");
        assert_eq!(parsed["id"], "1");
        assert_eq!(parsed["value"], serde_json::json!({"t": "f", "v": 1.0}));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_set_request_writes_and_is_visible_to_rust() {
        // The shared-state claim, at the smallest scale that can express it.
        let rt = rt();
        let handle = rt.handle();
        let reply = handle_ipc_text(
            &handle,
            SubscriberId(1),
            r#"{"kind":"set","id":"2","path":"editor.zoom","value":{"t":"f","v":4.5}}"#,
        );
        assert!(reply.contains("\"done\""), "expected a done response, got {reply}");
        assert_eq!(
            handle
                .get(&Path::parse("editor.zoom").expect("path"))
                .expect("read should succeed"),
            Some(Value::Float(4.5)),
            "a value written over IPC must be readable from Rust"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn malformed_ipc_text_produces_a_failed_response_not_a_panic() {
        // This is untrusted guest input arriving on a channel we do not control.
        let rt = rt();
        for bad in [
            "",
            "not json",
            "42",
            "{}",
            r#"{"kind":"nope","id":"1"}"#,
            r#"{"kind":"get","id":"1","path":"a.[0]"}"#,
            r#"{"kind":"get","id":1,"path":"a"}"#,
        ] {
            let reply = handle_ipc_text(&rt.handle(), SubscriberId(1), bad);
            let parsed: serde_json::Value = serde_json::from_str(&reply)
                .unwrap_or_else(|e| panic!("reply for {bad:?} must be valid JSON: {e}"));
            assert_eq!(
                parsed["kind"], "failed",
                "input {bad:?} should produce a failed response, got {reply}"
            );
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_unparseable_request_still_echoes_an_id_when_one_is_present() {
        // A guest correlates by id. If we cannot decode the body but the id is
        // readable, echo it so the guest can fail that specific call rather
        // than waiting forever.
        let rt = rt();
        let reply = handle_ipc_text(
            &rt.handle(),
            SubscriberId(1),
            r#"{"kind":"get","id":"77","path":"a.[0]"}"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["kind"], "failed");
        assert_eq!(parsed["id"], "77", "the failure must name the request it answers");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_handler_ignores_any_subscriber_named_on_the_wire() {
        // The security property: a guest cannot subscribe as another guest.
        // Even with a `subscriber` field present, the handler must use the one
        // it was constructed with.
        let rt = rt();
        let handle = rt.handle();
        let reply = handle_ipc_text(
            &handle,
            SubscriberId(7),
            r#"{"kind":"subscribe","id":"3","path":"editor.zoom","subscriber":"999"}"#,
        );
        assert!(reply.contains("\"subscribed\""), "got {reply}");

        // The subscription must belong to 7, not 999.
        assert_eq!(
            handle.drop_subscriber(SubscriberId(999)).expect("query"),
            0,
            "no subscription may be attributed to a guest-named subscriber"
        );
        assert_eq!(
            handle.drop_subscriber(SubscriberId(7)).expect("query"),
            1,
            "the subscription must belong to the host-supplied subscriber"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_push_is_framed_as_js_that_calls_the_guest_hook() {
        let note = duet_core::Notification {
            subscriber: SubscriberId(1),
            subscription: duet_core::SubscriptionId(2),
            patch: duet_core::Patch {
                path: Path::parse("editor.zoom").expect("path"),
                value: Value::Float(2.0),
            },
        };
        let js = push_script(&duet_protocol::Push::Notification(note));
        assert!(
            js.contains("__duet.onPush"),
            "a push must call the guest's hook, got {js}"
        );
        assert!(
            js.contains("\"t\":\"f\""),
            "the payload must be the tagged encoding, got {js}"
        );
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p duet-backend-macos`
Expected: FAIL — `cannot find function handle_ipc_text in this scope`.

- [ ] **Step 4: Write the implementation**

Insert above the test module in `crates/duet-backend-macos/src/webview.rs`:

```rust
use duet_core::SubscriberId;
use duet_protocol::{Push, RequestId, Response};
use duet_runtime::StoreHandle;

/// Serves one IPC message and returns the JSON text to send back.
///
/// Total by construction: malformed input becomes a [`Response::Failed`], so a
/// guest always receives well-formed JSON. `duet_protocol::dispatch` is itself
/// infallible, so the only failures possible here are decoding ones.
///
/// `subscriber` is the surface's own, supplied by the host. A `subscriber`
/// field appearing in the message is **ignored** — `duet_protocol::Request`
/// has no such field, so a guest cannot subscribe as another guest.
pub(crate) fn handle_ipc_text(
    store: &StoreHandle,
    subscriber: SubscriberId,
    text: &str,
) -> String {
    let response = match decode(text) {
        Ok(request) => duet_protocol::dispatch(store, subscriber, request),
        Err((id, message)) => Response::Failed { id, message },
    };
    // `encode_response` produces a plain JSON object; serializing it here is
    // the single encoding step. Note `wry`'s `evaluate_script_with_callback`
    // re-serializes anything a script *returns*, which is why responses are
    // pushed rather than returned — see `response_script`.
    serde_json::to_string(&duet_protocol::encode_response(&response))
        .unwrap_or_else(|_| FALLBACK_FAILURE.to_string())
}

/// Emitted only if serializing a `Response` itself fails, which cannot happen
/// for the shapes `encode_response` produces. Present so this function can be
/// total without an `expect`.
const FALLBACK_FAILURE: &str =
    r#"{"kind":"failed","id":"0","message":"host could not serialize its response"}"#;

/// Decodes a request, recovering the correlation id where possible.
///
/// A guest waits on its request id. When the body is undecodable but the id is
/// readable, returning it lets the guest fail that specific call instead of
/// hanging.
fn decode(text: &str) -> Result<duet_protocol::Request, (RequestId, String)> {
    let json: serde_json::Value = match serde_json::from_str(text) {
        Ok(j) => j,
        Err(e) => return Err((RequestId(0), format!("malformed JSON: {e}"))),
    };
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .map(RequestId)
        .unwrap_or(RequestId(0));

    duet_protocol::decode_request(&json).map_err(|e| (id, e.to_string()))
}

/// Wraps a response in JavaScript that hands it to the guest.
pub(crate) fn response_script(reply_json: &str) -> String {
    format!("window.__duet && window.__duet.onResponse({reply_json});")
}

/// Wraps a push in JavaScript that hands it to the guest.
pub(crate) fn push_script(push: &Push) -> String {
    let encoded = serde_json::to_string(&duet_protocol::encode_push(push))
        .unwrap_or_else(|_| "null".to_string());
    format!("window.__duet && window.__duet.onPush({encoded});")
}
```

- [ ] **Step 5: Declare the module**

Add `mod webview;` to `crates/duet-backend-macos/src/lib.rs`. Keep it private for now — Task 2 adds the public surface. Note `lib.rs` carries `#![deny(missing_docs)]`, so every public item needs a doc comment even inside a private module once it is re-exported.

- [ ] **Step 6: Run and commit**

Run: `cargo test -p duet-backend-macos`
Expected: PASS — **7 passed, 1 ignored** (2 pre-existing + 5 new).

```bash
git add crates/duet-backend-macos/src/
git commit -m "feat(backend-macos): route webview IPC through duet-protocol"
```

---

## Task 2: The guest bootstrap and `WebviewSurface`

**Files:**
- Create: `crates/duet-backend-macos/src/webview_html.rs`
- Modify: `crates/duet-backend-macos/src/webview.rs`, `crates/duet-backend-macos/src/lib.rs`

- [ ] **Step 1: Write the guest bootstrap**

Create `crates/duet-backend-macos/src/webview_html.rs`:

```rust
//! The HTML and JavaScript a webview guest boots with.

/// A minimal guest client: `__duet.get/set/subscribe`, correlated by request id.
///
/// Phase 4's codegen will generate a typed client over this same protocol; this
/// is the hand-written floor that proves the transport works.
pub(crate) const BOOTSTRAP_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Duet webview surface</title></head>
<body style="font-family: system-ui; padding: 1rem">
<h1>Duet webview surface</h1>
<pre id="log">booting…</pre>
<script>
(function () {
  const pending = new Map();
  let nextId = 1;

  function send(msg) {
    // wry delivers this to the Rust ipc_handler as a string.
    window.ipc.postMessage(JSON.stringify(msg));
  }

  function call(kind, extra) {
    const id = String(nextId++);
    return new Promise((resolve) => {
      pending.set(id, resolve);
      send(Object.assign({ kind, id }, extra));
    });
  }

  window.__duet = {
    // Resolved by the host calling back into onResponse.
    get: (path) => call("get", { path }),
    set: (path, value) => call("set", { path, value }),
    subscribe: (path) => call("subscribe", { path }),
    pushes: [],
    log: [],

    onResponse(response) {
      const resolve = pending.get(response.id);
      if (resolve) {
        pending.delete(response.id);
        resolve(response);
      }
      window.__duet.log.push(response);
      document.getElementById("log").textContent =
        JSON.stringify(window.__duet.log, null, 1);
    },

    onPush(push) {
      window.__duet.pushes.push(push);
    },
  };

  document.getElementById("log").textContent = "ready";
})();
</script>
</body>
</html>
"#;
```

Note `window.ipc.postMessage` — that is the channel `wry`'s `with_ipc_handler` receives on.

- [ ] **Step 2: Write the failing test**

Add inside `mod tests` in `crates/duet-backend-macos/src/webview.rs`:

```rust
    #[test]
    fn the_bootstrap_defines_the_hooks_the_host_calls() {
        // The host emits `window.__duet.onResponse(...)` and
        // `window.__duet.onPush(...)`. If the bootstrap stops defining either,
        // every reply is silently dropped — the guest would simply hang.
        let html = crate::webview_html::BOOTSTRAP_HTML;
        assert!(html.contains("onResponse"), "bootstrap must define onResponse");
        assert!(html.contains("onPush"), "bootstrap must define onPush");
        assert!(
            html.contains("window.ipc.postMessage"),
            "bootstrap must send on wry's IPC channel"
        );
    }

    #[test]
    fn a_response_script_targets_the_hook_the_bootstrap_defines() {
        // Pins the two halves against each other: if either the emitted JS or
        // the bootstrap's hook name changes alone, this fails.
        let script = response_script(r#"{"kind":"done","id":"1"}"#);
        assert!(script.contains("__duet.onResponse"), "got {script}");
        assert!(
            crate::webview_html::BOOTSTRAP_HTML.contains("onResponse"),
            "the bootstrap must define what the script calls"
        );
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p duet-backend-macos`
Expected: FAIL — `cannot find crate::webview_html`.

- [ ] **Step 4: Add the reply event**

`wry`'s IPC handler is supplied to the builder *before* `build()` returns the `WebView`, so the handler cannot hold the webview it must reply through. Route replies back through the event loop instead — the same `EventLoopProxy` mechanism Spike B measured at 709/709 delivered.

In `crates/duet-backend-macos/src/sink.rs`, add a variant to `DuetEvent`:

```rust
    /// JavaScript the host wants evaluated in a webview surface — an IPC
    /// reply, or a push.
    ///
    /// `wry`'s IPC handler is installed before `build()` hands back the
    /// `WebView`, so the handler cannot hold the webview it replies through.
    /// It posts this instead and the event loop, which does own the webview,
    /// evaluates it on the next turn.
    WebviewScript(String),
```

`examples/lifecycle.rs` matches `DuetEvent` with a trailing `_ => {}` arm (line 358), so it needs no change.

- [ ] **Step 5: Add `WebviewSurface`**

Add `mod webview_html;` to `lib.rs`, then add to `crates/duet-backend-macos/src/webview.rs`:

```rust
use duet_host::BackendError;
use duet_runtime::StoreHandle;
use tao::event_loop::EventLoopProxy;
use tao::window::Window;
use wry::{WebView, WebViewBuilder};

use crate::sink::DuetEvent;

/// A `wry` webview that speaks `duet-protocol` to the shared store.
///
/// Its IPC handler holds the surface's own [`SubscriberId`], captured at
/// construction and never read from a message, so one guest cannot subscribe
/// as another.
pub struct WebviewSurface {
    webview: WebView,
}

impl WebviewSurface {
    /// Builds a webview in `window`, wired to `store` as `subscriber`.
    ///
    /// Replies are posted to `proxy` as [`DuetEvent::WebviewScript`]; the
    /// caller's event loop must pass each one to [`WebviewSurface::eval`].
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `wry` could not create the webview.
    pub fn new(
        window: &Window,
        store: StoreHandle,
        subscriber: SubscriberId,
        proxy: EventLoopProxy<DuetEvent>,
    ) -> Result<Self, BackendError> {
        // The handler owns clones of everything it needs and borrows nothing
        // from this call — required, since it outlives `new`.
        let handler_store = store.clone();
        let webview = WebViewBuilder::new()
            .with_html(crate::webview_html::BOOTSTRAP_HTML)
            .with_ipc_handler(move |request| {
                let reply = handle_ipc_text(&handler_store, subscriber, request.body());
                // Replies are *pushed* into the guest, never returned from an
                // evaluated script: `wry` re-serializes a script's return
                // value, which would double-encode the JSON. Spike B hit
                // exactly that.
                //
                // A send failure means the event loop has already exited, so
                // there is no guest left to answer — dropping is correct.
                let _ = proxy.send_event(DuetEvent::WebviewScript(response_script(&reply)));
            })
            .build(window)
            .map_err(|e| BackendError::Unavailable(format!("webview: {e}")))?;

        Ok(WebviewSurface { webview })
    }

    /// Delivers a push to the guest.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the script could not be evaluated.
    pub fn push(&self, push: &Push) -> Result<(), BackendError> {
        self.eval(&push_script(push))
    }

    /// Evaluates JavaScript in the guest.
    ///
    /// The event loop calls this with the payload of every
    /// [`DuetEvent::WebviewScript`] it receives.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the script could not be evaluated.
    pub fn eval(&self, script: &str) -> Result<(), BackendError> {
        self.webview
            .evaluate_script(script)
            .map_err(|e| BackendError::Unavailable(format!("eval: {e}")))
    }
}
```

- [ ] **Step 6: Export and commit**

Add `pub use webview::WebviewSurface;` to `lib.rs`.

Run: `cargo test -p duet-backend-macos` and `cargo clippy -p duet-backend-macos --all-targets -- -D warnings`
Expected: PASS — **9 passed, 1 ignored**; clippy clean.

```bash
git add crates/duet-backend-macos/src/
git commit -m "feat(backend-macos): add WebviewSurface with a guest bootstrap"
```

---

## Task 3: The proof — JS writes, Rust reads

**Files:**
- Create: `crates/duet-backend-macos/examples/webview_state.rs`

An example, not a test: it needs the main thread and a real event loop.

- [ ] **Step 1: Write it**

Model it on `crates/duet-backend-macos/examples/lifecycle.rs`, which already drives a `tao` loop through a staged `Step` enum, one stage per run-loop turn. Reuse that structure — the staging is what makes an asynchronous sequence assertable.

It must:

1. Build the loop with `EventLoopBuilder::<DuetEvent>::with_user_event().build()`.
2. Spawn a `Runtime` seeded with `Value::map([("counter", Value::Int(0))])`, using a `ProxySink` over that loop's proxy.
3. Create a `tao` window and a `WebviewSurface` over it, passing `runtime.next_subscriber_id()` and a second clone of the proxy.
4. **Handle `Event::UserEvent(DuetEvent::WebviewScript(js))` by calling `surface.eval(&js)`.** Without this arm every IPC reply is dropped on the floor and the guest hangs forever — that arm *is* the reply path.
5. Once the page is up, `eval` JS calling `window.__duet.set("counter", {t:"i", v:"42"})`. Note `Int` is encoded as a **decimal string**, not a JSON number — that is the 2^53 guard from Phase 2b-0.
6. Pump the loop until the reply lands.
7. **Read `counter` from Rust and assert it is exactly `Value::Int(42)`** — not merely "changed", not "non-zero".
8. Then have JS `subscribe("counter")`, write `Value::Int(99)` **from Rust**, and assert the guest's `window.__duet.pushes` reaches length 1 carrying 99 — proving the notification path end to end, in the opposite direction.
9. Print a PASS/FAIL line per assertion and `std::process::exit(1)` on any failure, so a regression cannot pass silently.

For reading values back out of the page use `evaluate_script_with_callback` and have the JS return a **plain object** — returning an already-stringified JSON string makes `wry` serialize it twice. Spike B lost real time to exactly that.

Guard against a hang: cap the run with a deadline (lifecycle.rs uses `ControlFlow::WaitUntil`) and fail loudly if the sequence does not complete. A hanging example is indistinguishable from a slow one in CI.

- [ ] **Step 2: Run it**

```bash
cargo run -p duet-backend-macos --example webview_state
```

**Report the actual output.** If JS cannot reach the store, or the push never arrives, that is a finding about the transport — report it rather than adjusting the example until it passes.

- [ ] **Step 3: Commit**

```bash
git add crates/duet-backend-macos/examples/
git commit -m "feat(backend-macos): prove a JS guest shares the store"
```

---

## Task 4: Findings and verification

**Files:**
- Modify: `crates/duet-backend-macos/FINDINGS.md`

- [ ] **Step 1: Record what happened**

Append a section covering:

- Whether a JS-written value was readable from Rust, with the actual output.
- Whether a Rust-written value reached the guest as a push.
- Which `REPLY_SINK` resolution you chose and why.
- Whether `wry`'s double-encoding bit you, and how you avoided it.
- **What could not be verified here** — real mouse and keyboard input above all. Spike B found synthetic events reach a Flutter view but not a `WKWebView`, still unexplained.
- Anything contradicting the spec or the Phase 0 findings.

- [ ] **Step 2: Verify the rest of the workspace is untouched**

```bash
cargo test --workspace --exclude duet-backend-macos --locked
cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90
cargo clippy --workspace --exclude duet-backend-macos --all-targets --locked -- -D warnings
cargo fmt --all -- --check
git diff --stat main -- crates/duet-core crates/duet-runtime crates/duet-codec crates/duet-supervisor crates/duet-host crates/duet-protocol
```

All must pass; the last must be empty.

- [ ] **Step 3: Commit**

```bash
git add crates/duet-backend-macos/FINDINGS.md
git commit -m "docs(backend-macos): record webview surface findings"
```

---

## Done criteria

- [ ] `cargo test -p duet-backend-macos` passes — 9 passed, 1 ignored
- [ ] `serde_json` in `duet-backend-macos/Cargo.toml` carries `features = ["float_roundtrip"]`
- [ ] `cargo run -p duet-backend-macos --example webview_state` runs and prints its result
- [ ] **A value written by JS is readable from Rust** — with the actual output
- [ ] **A value written by Rust reaches the guest as a push** — with the actual output
- [ ] `cargo clippy -p duet-backend-macos --all-targets -- -D warnings` clean
- [ ] The other six crates are unchanged and still pass their gate
- [ ] `FINDINGS.md` records what could **not** be verified as explicitly as what could
- [ ] The IPC handler never reads a `subscriber` from the wire — pinned by `the_handler_ignores_any_subscriber_named_on_the_wire`

## What Phase 2b-5 deliberately does not build

- **The Flutter platform-channel transport.** Different framing constraints; a separate increment consuming the same `duet-protocol`.
- **A typed TypeScript client.** Phase 4's codegen generates it over this protocol. The bootstrap here is the hand-written floor that proves the transport.
- **Request batching or pipelining.** No benchmark exists; `Request` is `#[non_exhaustive]`, so a `Batch` variant is additive.
- **Capability scoping.** Every guest currently sees the whole store. `dispatch` taking the `SubscriberId` from the host is the seam where scoping would attach, but the policy model is not designed.
